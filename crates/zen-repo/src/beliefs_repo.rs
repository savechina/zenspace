use crate::client::{Result, SqliteClient, SqliteError};
use crate::types::{BeliefNodeRow, UpsertBeliefNodeRequest};

pub struct BeliefsRepo<'a> {
    client: &'a SqliteClient,
}

impl<'a> BeliefsRepo<'a> {
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
    }

    pub async fn upsert(&self, req: &UpsertBeliefNodeRequest<'_>) -> Result<()> {
        let id = req.id.to_string();
        let name = req.name.to_string();
        let proposition = req.proposition.to_string();
        let prior = req.prior;
        let posterior = req.posterior;
        let evidence_count_int = req.evidence_count as i64;
        let now = req.now.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO belief_nodes (id, name, proposition, prior, posterior, evidence_count, last_updated) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![id, name, proposition, prior, posterior, evidence_count_int, now],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn load(&self, id: &str) -> Result<Option<BeliefNodeRow>> {
        Ok(sqlx::query_as::<_, BeliefNodeRow>(
            "SELECT id, name, proposition, prior, posterior, evidence_count FROM belief_nodes WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(self.client.pool())
        .await?)
    }
}
