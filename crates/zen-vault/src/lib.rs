pub mod brief;
pub mod dispatch;
pub mod distill;
pub mod goal;
pub mod habit;
pub mod ingest;
pub mod intent;
pub mod note;
pub mod notion;
pub mod search;
pub mod tindy;
pub mod tools;
pub mod wiki;

pub use distill::{
    ChatImporter, Checkpoint, CheckpointManager, Contradiction, ContradictionDetector,
    DistillationPipeline, DistillationPipelineInput, DistillationReport, NotionExtractor,
    RecoveryManager, SourceIngester, TransactionScope, WikiCompiler,
};
pub use ingest::{
    FeedEntry, IngestResult, RssFetcher, extract_readable_content, fetch_feed, ingest_local_file,
    ingest_url,
};
pub use note::{Domain, Note, NoteService, parse_frontmatter, write_note};
pub use notion::{
    Notion, NotionData, NotionGraphAdapter, NotionKind, NotionService, RelationKind, Relationship,
};
pub use search::{
    GraphResult, SearchResult, SearchService, Tier1Search, Tier2Search, Tier3Search, Tier4Search,
    Tier5Search, TierSelector,
};
pub use tindy::{
    ChangeDetector, ComputeEmbeddings, EmbeddingResult, GapType, KnowledgeGap, LearningLoop,
    LearningReport, LintReportGenerator, LintResult, Linter, ReindexReport, Reindexer,
    ResearchTask, compute_embeddings, compute_embeddings_for_text, compute_file_checksum,
    needs_reindex, reindex_all, update_checksum,
};
pub use wiki::{WikiIndex, WikiLog, WikiPage, WikiStructure};
pub use zen_repo::SqliteClient;
