use zen_core::types::Sensitivity;

use crate::models::{AgentProfile, AuditLog, Note};

use async_trait::async_trait;

#[async_trait]
pub trait NoteRepository: Send + Sync {
    async fn insert(&self, note: &Note) -> Result<Note, crate::DataError>;
    async fn find_by_session(&self, session_id: &str) -> Result<Vec<Note>, crate::DataError>;
    async fn update_sensitivity(
        &self,
        note_id: &str,
        sensitivity: Sensitivity,
    ) -> Result<Note, crate::DataError>;
    async fn delete(&self, note_id: &str) -> Result<bool, crate::DataError>;
}

#[async_trait]
pub trait AgentProfileRepository: Send + Sync {
    async fn insert(&self, profile: &AgentProfile) -> Result<AgentProfile, crate::DataError>;
    async fn find_all(&self) -> Result<Vec<AgentProfile>, crate::DataError>;
    async fn find_by_name(&self, name: &str) -> Result<Option<AgentProfile>, crate::DataError>;
    async fn update(&self, profile: &AgentProfile) -> Result<AgentProfile, crate::DataError>;
}

#[async_trait]
pub trait AuditLogRepository: Send + Sync {
    async fn insert(&self, log: &AuditLog) -> Result<AuditLog, crate::DataError>;
    async fn find_by_session(&self, session_id: &str) -> Result<Vec<AuditLog>, crate::DataError>;
    async fn stream_to_file(
        &self,
        session_id: &str,
        output_path: &std::path::Path,
    ) -> Result<usize, crate::DataError>;
}
