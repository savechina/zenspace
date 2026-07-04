use sqlx::Row;

use crate::client::{Result, SqliteClient, SqliteError};
use crate::types::{EntityRow, GraphSearchResult, InsertRelationshipRequest, RelationshipRow};

pub fn normalize_alias(raw: &str) -> String {
    let mut s = raw.trim().to_lowercase();

    for suffix in &[".js", ".rs", ".py", "-lang", " language", ".ts", ".go", ".java", ".rb"] {
        if let Some(stripped) = s.strip_suffix(suffix) {
            s = stripped.to_string();
            break;
        }
    }

    s.trim().to_string()
}

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
        self.insert_entity_with(id, name, entity_type, now, "", "manual", 0.5)
            .await
    }

    pub async fn insert_entity_with(
        &self,
        id: &str,
        name: &str,
        entity_type: &str,
        now: &str,
        description: &str,
        source: &str,
        confidence: f64,
    ) -> Result<()> {
        let id = id.to_string();
        let name = name.to_string();
        let entity_type = entity_type.to_string();
        let now = now.to_string();
        let description = description.to_string();
        let source = source.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO entities \
                     (id, name, entity_type, created_at, last_updated, description, source, confidence) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![id, name, entity_type, now, now, description, source, confidence],
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
        self.upsert_entity_with(id, name, entity_type, created_at, last_updated, "", "manual", 0.5)
            .await
    }

    pub async fn upsert_entity_with(
        &self,
        id: &str,
        name: &str,
        entity_type: &str,
        created_at: &str,
        last_updated: &str,
        description: &str,
        source: &str,
        confidence: f64,
    ) -> Result<()> {
        let id = id.to_string();
        let name = name.to_string();
        let entity_type = entity_type.to_string();
        let created_at = created_at.to_string();
        let last_updated = last_updated.to_string();
        let description = description.to_string();
        let source = source.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO entities \
                     (id, name, entity_type, created_at, last_updated, description, source, confidence) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                     ON CONFLICT(name, entity_type) DO UPDATE SET \
                     last_updated = ?5, \
                     description = CASE WHEN excluded.description != '' THEN excluded.description ELSE entities.description END, \
                     confidence = CASE WHEN excluded.confidence != 0.5 THEN excluded.confidence ELSE entities.confidence END",
                    rusqlite::params![id, name, entity_type, created_at, last_updated, description, source, confidence],
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

    pub async fn update_entity_access(&self, entity_id: &str) -> Result<()> {
        let entity_id = entity_id.to_string();
        let now = chrono::Utc::now().to_rfc3339();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "UPDATE entities \
                     SET access_count = access_count + 1, last_accessed_at = ?1 \
                     WHERE id = ?2",
                    rusqlite::params![now, entity_id],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn update_entity_confidence(
        &self,
        entity_id: &str,
        confidence: f64,
    ) -> Result<()> {
        let entity_id = entity_id.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "UPDATE entities SET confidence = ?1 WHERE id = ?2",
                    rusqlite::params![confidence, entity_id],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn promote_entity(&self, entity_id: &str) -> Result<()> {
        let entity_id = entity_id.to_string();
        let now = chrono::Utc::now().to_rfc3339();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "UPDATE entities SET promoted_at = ?1 WHERE id = ?2 AND promoted_at IS NULL",
                    rusqlite::params![now, entity_id],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn insert_alias(&self, alias: &str, canonical_entity_id: &str) -> Result<()> {
        let alias = normalize_alias(alias);
        if alias.is_empty() {
            return Ok(());
        }
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

    pub async fn load_aliases_for_entity(&self, entity_id: &str) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT alias FROM entity_aliases WHERE canonical_entity_id = ?1 ORDER BY alias",
        )
        .bind(entity_id)
        .fetch_all(self.client.pool())
        .await?;
        Ok(rows.iter().map(|row| row.get::<String, _>(0)).collect())
    }

    pub async fn insert_relationship(&self, req: &InsertRelationshipRequest<'_>) -> Result<()> {
        let id = req.id.to_string();
        let source_id = req.source_id.to_string();
        let target_id = req.target_id.to_string();
        let rel_type = req.rel_type.to_string();
        let confidence = req.confidence;
        let source_note_ids = req.source_note_ids.unwrap_or("").to_string();
        let created_at = req.created_at.to_string();
        let description = req.description.unwrap_or("").to_string();
        let valid_from = req.valid_from.map(|s| s.to_string());
        let valid_until = req.valid_until.map(|s| s.to_string());
        let weight = req.weight.unwrap_or(1.0);

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO relationships \
                     (id, source_entity_id, target_entity_id, relation_type, confidence, \
                      source_note_ids, created_at, description, valid_from, valid_until, weight) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    rusqlite::params![
                        id, source_id, target_id, rel_type, confidence,
                        source_note_ids, created_at, description, valid_from, valid_until, weight
                    ],
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
            "SELECT id, name, entity_type, created_at, domain, last_updated, \
             description, properties, access_count, last_accessed_at, \
             confidence, source, promoted_at \
             FROM entities ORDER BY name",
        )
        .fetch_all(self.client.pool())
        .await?)
    }

    pub async fn load_entities_updated_since(&self, since: &str) -> Result<Vec<EntityRow>> {
        Ok(sqlx::query_as::<_, EntityRow>(
            "SELECT id, name, entity_type, created_at, domain, last_updated, \
             description, properties, access_count, last_accessed_at, \
             confidence, source, promoted_at \
             FROM entities WHERE last_updated > ?1 ORDER BY name",
        )
        .bind(since)
        .fetch_all(self.client.pool())
        .await?)
    }

    pub async fn resolve_alias(&self, alias: &str) -> Result<Option<String>> {
        let alias = normalize_alias(alias);
        let result = sqlx::query("SELECT canonical_entity_id FROM entity_aliases WHERE alias = ?1")
            .bind(alias)
            .fetch_optional(self.client.pool())
            .await?;
        Ok(result.map(|row| row.get::<String, _>(0)))
    }

    pub async fn load_relationships(&self, entity_id: &str) -> Result<Vec<RelationshipRow>> {
        Ok(sqlx::query_as::<_, RelationshipRow>(
            "SELECT id, source_entity_id, target_entity_id, relation_type, confidence, \
             source_note_ids, created_at, description, valid_from, valid_until, \
             recorded_at, weight \
             FROM relationships WHERE source_entity_id = ?1",
        )
        .bind(entity_id)
        .fetch_all(self.client.pool())
        .await?)
    }

    pub async fn load_relationships_all(&self, entity_id: &str) -> Result<Vec<RelationshipRow>> {
        Ok(sqlx::query_as::<_, RelationshipRow>(
            "SELECT id, source_entity_id, target_entity_id, relation_type, confidence, \
             source_note_ids, created_at, description, valid_from, valid_until, \
             recorded_at, weight \
             FROM relationships WHERE source_entity_id = ?1 OR target_entity_id = ?1",
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

    pub async fn search_entities_fts(&self, query: &str) -> Result<Vec<EntityRow>> {
        let query = query.trim();
        if query.is_empty() {
            return self.load_all_entities().await;
        }

        let fts_query = format!("{}*", query.replace('"', "''"));

        Ok(sqlx::query_as::<_, EntityRow>(
            "SELECT e.id, e.name, e.entity_type, e.created_at, e.domain, e.last_updated, \
             e.description, e.properties, e.access_count, e.last_accessed_at, \
             e.confidence, e.source, e.promoted_at \
             FROM entities_fts f \
             JOIN entities e ON e.id = f.entity_id \
             WHERE entities_fts MATCH ?1 \
             ORDER BY rank \
             LIMIT 50",
        )
        .bind(fts_query)
        .fetch_all(self.client.pool())
        .await?)
    }

    pub async fn bfs_search(
        &self,
        entity_name: &str,
        max_depth: u32,
    ) -> Result<Vec<GraphSearchResult>> {
        self.bfs_search_filtered(entity_name, max_depth, "").await
    }

    pub async fn bfs_search_filtered(
        &self,
        entity_name: &str,
        max_depth: u32,
        relation_type_filter: &str,
    ) -> Result<Vec<GraphSearchResult>> {
        if entity_name.trim().is_empty() {
            return Ok(Vec::new());
        }

        let rel_filter = relation_type_filter.to_string();

        let rows = sqlx::query(
            "WITH RECURSIVE \
             edge_set(from_id, to_id, relation_type, direction) AS ( \
                 SELECT source_entity_id, target_entity_id, relation_type, 'outbound' \
                 FROM relationships \
                 WHERE (valid_until IS NULL OR valid_until = '') \
                   AND (?3 = '' OR relation_type = ?3) \
                 UNION ALL \
                 SELECT target_entity_id, source_entity_id, relation_type, 'inbound' \
                 FROM relationships \
                 WHERE (valid_until IS NULL OR valid_until = '') \
                   AND (?3 = '' OR relation_type = ?3) \
             ), \
             bfs(id, name, depth, source_name, relation_type, direction, path) AS ( \
                 SELECT id, name, 0, CAST('' AS TEXT), CAST('' AS TEXT), CAST('' AS TEXT), \
                        ',' || id || ',' \
                 FROM entities WHERE name = ?1 \
                 UNION ALL \
                 SELECT e.id, e.name, b.depth + 1, b.name, edge.relation_type, edge.direction, \
                        b.path || e.id || ',' \
                 FROM bfs b \
                 JOIN edge_set edge ON edge.from_id = b.id \
                 JOIN entities e ON e.id = edge.to_id \
                 WHERE b.depth < ?2 \
                   AND instr(b.path, ',' || e.id || ',') = 0 \
             ) \
             SELECT \
                 b.name as entity, \
                 b.depth as depth, \
                 MIN(b.relation_type) as relation, \
                 b.name as target, \
                 MIN(b.source_name) as source_entity, \
                 MIN(b.direction) as direction \
             FROM bfs b \
             WHERE b.depth > 0 \
             GROUP BY b.name, b.depth \
             ORDER BY b.depth, b.name",
        )
        .bind(entity_name)
        .bind(max_depth)
        .bind(rel_filter)
        .fetch_all(self.client.pool())
        .await?;

        let results = rows
            .into_iter()
            .map(|row| GraphSearchResult {
                entity: row.get::<String, _>("entity"),
                depth: row.get::<i64, _>("depth") as u32,
                relation: row.get::<String, _>("relation"),
                target: row.get::<String, _>("target"),
                source_entity: row.get::<String, _>("source_entity"),
                direction: row.get::<String, _>("direction"),
            })
            .collect();

        Ok(results)
    }
}
