use sqlx::Row;

use crate::client::{Result, SqliteClient, SqliteError};

pub struct SessionsRepo<'a> {
    client: &'a SqliteClient,
}

impl<'a> SessionsRepo<'a> {
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn upsert(
        &self,
        id: &str,
        file_path: &str,
        agent_name: &str,
        status: &str,
        created_at: &str,
        updated_at: &str,
        workspace: &str,
    ) -> Result<()> {
        let id = id.to_string();
        let file_path = file_path.to_string();
        let agent_name = agent_name.to_string();
        let status = status.to_string();
        let created_at = created_at.to_string();
        let updated_at = updated_at.to_string();
        let workspace = workspace.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO sessions (id, file_path, agent_name, status, created_at, updated_at, workspace) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![id, file_path, agent_name, status, created_at, updated_at, workspace],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn find(&self, id: &str) -> Result<Option<String>> {
        let result = sqlx::query("SELECT file_path FROM sessions WHERE id = ?1")
            .bind(id)
            .fetch_optional(self.client.pool())
            .await?;
        Ok(result.map(|row| row.get::<String, _>(0)))
    }

    pub async fn list_all(&self) -> Result<Vec<IndexedSession>> {
        Ok(sqlx::query_as::<_, IndexedSession>(
            "SELECT id, file_path, agent_name, status, created_at, updated_at, workspace \
             FROM sessions ORDER BY updated_at DESC",
        )
        .fetch_all(self.client.pool())
        .await?)
    }

    pub async fn reconcile(&self, id: &str, new_path: &str) -> Result<()> {
        let id = id.to_string();
        let new_path = new_path.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "UPDATE sessions SET file_path = ?1 WHERE id = ?2",
                    rusqlite::params![new_path, id],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }
}

#[derive(sqlx::FromRow)]
pub struct IndexedSession {
    pub id: String,
    pub file_path: String,
    pub agent_name: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub workspace: String,
}
