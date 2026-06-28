use crate::client::{Result, SqliteClient, SqliteError};

pub struct DispatchRepo<'a> {
    client: &'a SqliteClient,
}

impl<'a> DispatchRepo<'a> {
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
    }

    pub async fn create_task(
        &self,
        id: &str,
        target: &str,
        task_description: &str,
        context_files: Option<&str>,
        created_at: &str,
    ) -> Result<()> {
        let id = id.to_string();
        let target = target.to_string();
        let task_description = task_description.to_string();
        let context_files = context_files.map(|s| s.to_string());
        let created_at = created_at.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO dispatch_tasks (id, target, task_description, context_files, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![id, target, task_description, context_files, created_at],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn update_status(
        &self,
        id: &str,
        status: &str,
        result_summary: Option<&str>,
        completed_at: Option<&str>,
    ) -> Result<()> {
        let id = id.to_string();
        let status = status.to_string();
        let result_summary = result_summary.map(|s| s.to_string());
        let completed_at = completed_at.map(|s| s.to_string());

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "UPDATE dispatch_tasks SET status = ?1, result_summary = ?2, completed_at = ?3 WHERE id = ?4",
                    rusqlite::params![status, result_summary, completed_at, id],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn load_task(&self, id: &str) -> Result<Option<DispatchTaskRow>> {
        Ok(sqlx::query_as::<_, DispatchTaskRow>(
            "SELECT id, target, task_description, status, context_files, result_summary, created_at, completed_at \
             FROM dispatch_tasks WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(self.client.pool())
        .await?)
    }

    pub async fn load_tasks_by_status(&self, status: &str) -> Result<Vec<DispatchTaskRow>> {
        Ok(sqlx::query_as::<_, DispatchTaskRow>(
            "SELECT id, target, task_description, status, context_files, result_summary, created_at, completed_at \
             FROM dispatch_tasks WHERE status = ?1 ORDER BY created_at",
        )
        .bind(status)
        .fetch_all(self.client.pool())
        .await?)
    }
}

#[derive(sqlx::FromRow)]
pub struct DispatchTaskRow {
    pub id: String,
    pub target: String,
    pub task_description: String,
    pub status: String,
    pub context_files: Option<String>,
    pub result_summary: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}
