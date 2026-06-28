use sqlx::Row;

use crate::client::{Result, SqliteClient, SqliteError};
use crate::types::{EntityRow, GraphSearchResult, InsertRelationshipRequest, RelationshipRow};

pub struct EntitiesRepo<'a> {
    client: &'a SqliteClient,
}

impl<'a> EntitiesRepo<'a> {
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
    }

    pub async fn insert_entity(
        &self,
        id: &str,
        name: &str,
        entity_type: &str,
        now: &str,
    ) -> Result<()> {
        let id = id.to_string();
        let name = name.to_string();
        let entity_type = entity_type.to_string();
        let now = now.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO entities (id, name, entity_type, created_at, last_updated) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![id, name, entity_type, now, now],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn upsert_entity(
        &self,
        id: &str,
        name: &str,
        entity_type: &str,
        created_at: &str,
        last_updated: &str,
    ) -> Result<()> {
        let id = id.to_string();
        let name = name.to_string();
        let entity_type = entity_type.to_string();
        let created_at = created_at.to_string();
        let last_updated = last_updated.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO entities (id, name, entity_type, created_at, last_updated) \
                     VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT(name, entity_type) DO UPDATE SET last_updated = ?5",
                    rusqlite::params![id, name, entity_type, created_at, last_updated],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn update_entity_timestamp(&self, entity_id: &str, last_updated: &str) -> Result<()> {
        let entity_id = entity_id.to_string();
        let last_updated = last_updated.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "UPDATE entities SET last_updated = ?1 WHERE id = ?2",
                    rusqlite::params![last_updated, entity_id],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn insert_alias(&self, alias: &str, canonical_entity_id: &str) -> Result<()> {
        let alias = alias.to_string();
        let canonical_entity_id = canonical_entity_id.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO entity_aliases (alias, canonical_entity_id) VALUES (?1, ?2)",
                    rusqlite::params![alias, canonical_entity_id],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn insert_relationship(&self, req: &InsertRelationshipRequest<'_>) -> Result<()> {
        let id = req.id.to_string();
        let source_id = req.source_id.to_string();
        let target_id = req.target_id.to_string();
        let rel_type = req.rel_type.to_string();
        let confidence = req.confidence;
        let source_note_ids = req.source_note_ids.unwrap_or("").to_string();
        let created_at = req.created_at.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO relationships \
                     (id, source_entity_id, target_entity_id, relation_type, confidence, source_note_ids, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![id, source_id, target_id, rel_type, confidence, source_note_ids, created_at],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn load_known_entity_names(&self) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT DISTINCT name FROM entities \
             UNION \
             SELECT DISTINCT alias FROM entity_aliases",
        )
        .fetch_all(self.client.pool())
        .await?;
        Ok(rows.iter().map(|row| row.get::<String, _>(0)).collect())
    }

    pub async fn load_all_entities(&self) -> Result<Vec<EntityRow>> {
        Ok(sqlx::query_as::<_, EntityRow>(
            "SELECT id, name, entity_type, created_at, domain, aliases, last_updated \
             FROM entities ORDER BY name",
        )
        .fetch_all(self.client.pool())
        .await?)
    }

    pub async fn load_entities_updated_since(&self, since: &str) -> Result<Vec<EntityRow>> {
        Ok(sqlx::query_as::<_, EntityRow>(
            "SELECT id, name, entity_type, created_at, domain, aliases, last_updated \
             FROM entities WHERE last_updated > ?1 ORDER BY name",
        )
        .bind(since)
        .fetch_all(self.client.pool())
        .await?)
    }

    pub async fn resolve_alias(&self, alias: &str) -> Result<Option<String>> {
        let result = sqlx::query("SELECT canonical_entity_id FROM entity_aliases WHERE alias = ?1")
            .bind(alias)
            .fetch_optional(self.client.pool())
            .await?;
        Ok(result.map(|row| row.get::<String, _>(0)))
    }

    pub async fn load_relationships(&self, entity_id: &str) -> Result<Vec<RelationshipRow>> {
        Ok(sqlx::query_as::<_, RelationshipRow>(
            "SELECT target_entity_id, relation_type FROM relationships WHERE source_entity_id = ?1",
        )
        .bind(entity_id)
        .fetch_all(self.client.pool())
        .await?)
    }

    pub async fn entity_name(&self, entity_id: &str) -> Result<Option<String>> {
        let result = sqlx::query("SELECT name FROM entities WHERE id = ?1")
            .bind(entity_id)
            .fetch_optional(self.client.pool())
            .await?;
        Ok(result.map(|row| row.get::<String, _>(0)))
    }

    pub async fn bfs_search(
        &self,
        entity_name: &str,
        max_depth: u32,
    ) -> Result<Vec<GraphSearchResult>> {
        if entity_name.trim().is_empty() {
            return Ok(Vec::new());
        }

        let rows = sqlx::query(
            "WITH RECURSIVE bfs(id, name, depth) AS (
                SELECT id, name, 0 FROM entities WHERE name = ?1
                UNION ALL
                SELECT e.id, e.name, b.depth + 1
                FROM bfs b
                JOIN relationships r ON r.source_entity_id = b.id
                JOIN entities e ON e.id = r.target_entity_id
                WHERE b.depth < ?2
            )
            SELECT
                b.name as entity,
                b.depth as depth,
                COALESCE(GROUP_CONCAT(DISTINCT r.relation_type), '') as relation,
                b.name as target
            FROM bfs b
            LEFT JOIN relationships r ON r.source_entity_id = b.id
            WHERE b.depth > 0
            GROUP BY b.id, b.name, b.depth
            ORDER BY b.depth, b.name",
        )
        .bind(entity_name)
        .bind(max_depth)
        .fetch_all(self.client.pool())
        .await?;

        let results = rows
            .into_iter()
            .map(|row| GraphSearchResult {
                entity: row.get::<String, _>("entity"),
                depth: row.get::<i64, _>("depth") as u32,
                relation: row.get::<String, _>("relation"),
                target: row.get::<String, _>("target"),
            })
            .collect();

        Ok(results)
    }
}
