use crate::client::{Result, SqliteClient, SqliteError};
use crate::types::{GoalNodeRow, PathNodeRow, UpsertGoalNodeRequest, UpsertPathNodeRequest};

pub struct GoalsRepo<'a> {
    client: &'a SqliteClient,
}

impl<'a> GoalsRepo<'a> {
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
    }

    pub async fn upsert_goal(&self, req: &UpsertGoalNodeRequest<'_>) -> Result<()> {
        let id = req.id.to_string();
        let name = req.name.to_string();
        let controllability = req.controllability;
        let core_pursuit = req.core_pursuit.to_string();
        let deadline = req.deadline.map(|s| s.to_string());
        let now = req.now.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO goal_nodes (id, name, controllability, core_pursuit, deadline, created_at, last_updated) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                    rusqlite::params![id, name, controllability, core_pursuit, deadline, now],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn upsert_path(&self, req: &UpsertPathNodeRequest<'_>) -> Result<()> {
        let id = req.id.to_string();
        let name = req.name.to_string();
        let serves_goal_id = req.serves_goal_id.map(|s| s.to_string());
        let is_default_int = req.is_default as i32;
        let crowdedness = req.crowdedness;
        let alternatives = req.alternatives.to_string();
        let now = req.now.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO path_nodes (id, name, serves_goal_id, is_default, crowdedness, alternatives, created_at, last_updated) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                    rusqlite::params![id, name, serves_goal_id, is_default_int, crowdedness, alternatives, now],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn load_goal(&self, id: &str) -> Result<Option<GoalNodeRow>> {
        Ok(sqlx::query_as::<_, GoalNodeRow>(
            "SELECT id, name, controllability, core_pursuit, deadline FROM goal_nodes WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(self.client.pool())
        .await?)
    }

    pub async fn load_path(&self, id: &str) -> Result<Option<PathNodeRow>> {
        Ok(sqlx::query_as::<_, PathNodeRow>(
            "SELECT id, name, serves_goal_id, is_default, crowdedness, alternatives FROM path_nodes WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(self.client.pool())
        .await?)
    }
}
