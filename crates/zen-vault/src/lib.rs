pub mod brief;
pub mod communities;
pub mod dispatch;
pub mod distill;
pub mod goal;
pub mod graph_router;
pub mod graph_verify;
pub mod habit;
pub mod ingest;
pub mod intent;
pub mod note;
pub mod notion;
pub mod search;
pub mod tindy;
pub mod tools;
pub use tools::SharedSqliteClient;
pub mod wiki;

pub use communities::{CommunitySummaryReport, run_community_summarization};
pub use distill::{
    Checkpoint, CheckpointManager, Contradiction, ContradictionDetector, DiscoveryNode,
    DiscoveryTree, DistillationPipeline, DistillationPipelineInput, DistillationReport,
    NotionExtractor, RecoveryManager, ScopedRunOutcome, SourceIngester, TransactionScope,
    WikiCompiler, discovery_tree_path, prune_context,
};
pub use graph_verify::{GraphIntegrityVerifier, wiki_page_inventory};
pub use ingest::{
    FeedEntry, IngestResult, RssFetcher, extract_readable_content, fetch_feed, ingest_local_file,
    ingest_url,
};
pub use note::{Domain, Note, NoteService, parse_frontmatter, write_note};
pub use notion::{
    Notion, NotionData, NotionGraphAdapter, NotionKind, NotionService, RelationKind, Relationship,
};
pub use search::{
    GraphResult, SearchResult, SearchService, Tier1Search, Tier2Search, Tier2SearchTool,
    Tier3Search, Tier3SearchTool, Tier4Search, Tier4SearchTool, Tier5Search, TierSelector,
};
pub use tindy::{
    ChangeDetector, ComputeEmbeddings, EmbeddingResult, GapType, KnowledgeGap, LearningLoop,
    LearningReport, LintReportGenerator, LintResult, Linter, ReindexReport, Reindexer,
    ResearchTask, compute_embeddings, compute_embeddings_for_text, compute_file_checksum,
    needs_reindex, reindex_all, update_checksum,
};
pub use wiki::{WikiIndex, WikiLog, WikiPage, WikiStructure};
pub use zen_repo::SqliteClient;
