pub mod checksum;
pub mod embeddings;
pub mod learning_loop;
pub mod lint;
pub mod lint_report;
pub mod local_embedder;
pub mod reindex;

pub use checksum::{ChangeDetector, compute_file_checksum, needs_reindex, update_checksum};
pub use embeddings::{
    ComputeEmbeddings, EmbeddingResult, compute_embeddings, compute_embeddings_for_text,
};
pub use learning_loop::{GapType, KnowledgeGap, LearningLoop, LearningReport, ResearchTask};
pub use lint::{LintResult, Linter};
pub use lint_report::LintReportGenerator;
pub use local_embedder::try_local_embed;
pub use reindex::{ReindexReport, Reindexer, reindex_all};
