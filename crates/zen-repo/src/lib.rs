use std::fmt;

pub mod models;
pub mod pool;
pub mod repo_impl;
pub mod repositories;
pub mod schema;
pub mod sqlite_repo;

pub use models::{AgentProfile, AuditLog, Note};
pub use pool::create_pool;
pub use repo_impl::{SqliteAgentProfileRepository, SqliteAuditLogRepository, SqliteNoteRepository};
pub use repositories::{AgentProfileRepository, AuditLogRepository, NoteRepository};
pub use sqlite_repo::{SqliteRepo, init_graph_schema, init_kb_schema, init_vec_schema};

#[derive(Debug)]
pub enum DataError {
    Database(sqlx::Error),
    NotFound(String),
}

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataError::Database(e) => write!(f, "database error: {e}"),
            DataError::NotFound(msg) => write!(f, "not found: {msg}"),
        }
    }
}

impl std::error::Error for DataError {}

impl From<sqlx::Error> for DataError {
    fn from(e: sqlx::Error) -> Self {
        DataError::Database(e)
    }
}

impl From<DataError> for zen_core::errors::ZenError {
    fn from(e: DataError) -> Self {
        zen_core::errors::ZenError::Service(e.to_string())
    }
}
