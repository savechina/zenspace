use std::collections::{HashMap, HashSet};

use sqlx::Row;
use unicode_normalization::UnicodeNormalization;

use crate::client::{Result, SqliteClient, SqliteError};
use crate::types::{
    Community, CommunityMember, ComponentResult, GraphSearchResult, InsertRelationshipRequest,
    NoteStageRow, NotionRow, PageRankResult, RelationRow, ShortestPathResult,
};

/// Canonical alias normalization (FR-022). Applied on both write
/// (`insert_alias`) and read (`resolve_alias`), so it is the single place that
/// decides whether two spellings of an entity are the same entity.
///
/// NFC comes first: a decomposed `e` + combining accent and a precomposed `é`
/// are different byte sequences but the same name, and without normalization
/// they would produce two aliases for one entity.
pub fn normalize_alias(raw: &str) -> String {
    let nfc: String = raw.nfc().collect();
    let mut s = nfc.trim().to_lowercase();

    for suffix in &[
        ".js",
        ".rs",
        ".py",
        "-lang",
        " lang",
        " language",
        ".ts",
        ".go",
        ".java",
        ".rb",
    ] {
        if let Some(stripped) = s.strip_suffix(suffix) {
            s = stripped.to_string();
            break;
        }
    }

    s.trim().to_string()
}

pub struct NotionsRepo<'a> {
    client: &'a SqliteClient,
}

/// Directed snapshot of currently-valid entity edges shared by the
/// [`pagerank`](NotionsRepo::pagerank) and
/// [`personalized_pagerank`](NotionsRepo::personalized_pagerank) iterations.
struct GraphCore {
    name_by_idx: Vec<String>,
    name_to_idx: HashMap<String, usize>,
    id_to_idx: HashMap<String, usize>,
    out_degree: Vec<usize>,
    inbound: Vec<Vec<usize>>,
}

/// Power-iteration core: `pr` starts at `teleport`, then each round applies
/// `pr[i] ← restart·teleport[i] + damping·(dangling_sum·teleport[i] +
/// Σ_{src→i} pr[src]/out[src])`. Dangling mass (nodes without out-edges)
/// redistributes along `teleport`, so a uniform teleport reproduces classic
/// PageRank. Returns all entities sorted by descending score.
fn run_pagerank_core(
    core: &GraphCore,
    teleport: Vec<f64>,
    iterations: usize,
    damping: f64,
    restart: f64,
) -> Vec<PageRankResult> {
    let n = core.name_by_idx.len();
    let mut pr = teleport.clone();

    for _ in 0..iterations {
        let dangling_sum: f64 = pr
            .iter()
            .enumerate()
            .filter(|(i, _)| core.out_degree[*i] == 0)
            .map(|(_, &score)| score)
            .sum();

        let mut new_pr = vec![0.0; n];
        for i in 0..n {
            new_pr[i] = (restart + damping * dangling_sum) * teleport[i];
            for &src_idx in &core.inbound[i] {
                if core.out_degree[src_idx] > 0 {
                    new_pr[i] += damping * pr[src_idx] / core.out_degree[src_idx] as f64;
                }
            }
        }

        pr = new_pr;
    }

    let mut results: Vec<PageRankResult> = pr
        .iter()
        .enumerate()
        .map(|(i, &score)| PageRankResult {
            notion: core.name_by_idx[i].clone(),
            score,
        })
        .collect();
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    results
}

impl<'a> NotionsRepo<'a> {
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
    }

    pub async fn insert_entity(&self, id: &str, name: &str, kind: &str, now: &str) -> Result<()> {
        self.insert_entity_with(id, name, kind, now, "", "manual", 0.5)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_entity_with(
        &self,
        id: &str,
        name: &str,
        kind: &str,
        now: &str,
        description: &str,
        source: &str,
        confidence: f64,
    ) -> Result<()> {
        let id = id.to_string();
        let name = name.to_string();
        let kind = kind.to_string();
        let now = now.to_string();
        let description = description.to_string();
        let source = source.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO notions \
                     (id, name, kind, created_at, last_updated, description, source, confidence) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![id, name, kind, now, now, description, source, confidence],
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
        kind: &str,
        created_at: &str,
        last_updated: &str,
    ) -> Result<()> {
        self.upsert_entity_with(id, name, kind, created_at, last_updated, "", "manual", 0.5)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_entity_with(
        &self,
        id: &str,
        name: &str,
        kind: &str,
        created_at: &str,
        last_updated: &str,
        description: &str,
        source: &str,
        confidence: f64,
    ) -> Result<()> {
        let id = id.to_string();
        let name = name.to_string();
        let kind = kind.to_string();
        let created_at = created_at.to_string();
        let last_updated = last_updated.to_string();
        let description = description.to_string();
        let source = source.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO notions \
                     (id, name, kind, created_at, last_updated, description, source, confidence) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                     ON CONFLICT(name, kind) DO UPDATE SET \
                     last_updated = ?5, \
                     description = CASE WHEN excluded.description != '' THEN excluded.description ELSE notions.description END, \
                     confidence = CASE WHEN excluded.confidence != 0.5 THEN excluded.confidence ELSE notions.confidence END",
                    rusqlite::params![id, name, kind, created_at, last_updated, description, source, confidence],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn update_entity_timestamp(&self, notion_id: &str, last_updated: &str) -> Result<()> {
        let notion_id = notion_id.to_string();
        let last_updated = last_updated.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "UPDATE notions SET last_updated = ?1 WHERE id = ?2",
                    rusqlite::params![last_updated, notion_id],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn update_entity_access(&self, notion_id: &str) -> Result<()> {
        let notion_id = notion_id.to_string();
        let now = chrono::Utc::now().to_rfc3339();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "UPDATE notions \
                     SET access_count = access_count + 1, last_accessed_at = ?1 \
                     WHERE id = ?2",
                    rusqlite::params![now, notion_id],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn update_entity_confidence(&self, notion_id: &str, confidence: f64) -> Result<()> {
        let notion_id = notion_id.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "UPDATE notions SET confidence = ?1 WHERE id = ?2",
                    rusqlite::params![confidence, notion_id],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn promote_entity(&self, notion_id: &str) -> Result<()> {
        let notion_id = notion_id.to_string();
        let now = chrono::Utc::now().to_rfc3339();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "UPDATE notions SET promoted_at = ?1 WHERE id = ?2 AND promoted_at IS NULL",
                    rusqlite::params![now, notion_id],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn insert_alias(&self, alias: &str, canonical_notion_id: &str) -> Result<()> {
        let alias = normalize_alias(alias);
        if alias.is_empty() {
            return Ok(());
        }
        let canonical_notion_id = canonical_notion_id.to_string();

        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO notion_aliases (alias, canonical_notion_id) VALUES (?1, ?2)",
                    rusqlite::params![alias, canonical_notion_id],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn load_aliases_for_entity(&self, notion_id: &str) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT alias FROM notion_aliases WHERE canonical_notion_id = ?1 ORDER BY alias",
        )
        .bind(notion_id)
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
                     (id, source_notion_id, target_notion_id, relation_type, confidence, \
                      source_note_ids, created_at, description, valid_from, valid_until, weight, \
                      t_valid) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?7)",
                    rusqlite::params![
                        id,
                        source_id,
                        target_id,
                        rel_type,
                        confidence,
                        source_note_ids,
                        created_at,
                        description,
                        valid_from,
                        valid_until,
                        weight
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    /// Temporal insert with contradiction resolution (Graphiti pattern).
    ///
    /// Records `t_valid` = `req.created_at` (RFC3339 UTC) and an open-ended
    /// `t_invalid` (NULL), then atomically soft-invalidates every currently
    /// open edge that shares (source_notion_id, relation_type) but points at a
    /// *different* target: its `t_invalid` becomes this edge's `t_valid`, since
    /// two open-ended windows `[t0, ∞)` always overlap temporally.
    ///
    /// Policy:
    /// - Rows are never deleted — superseded facts stay queryable via
    ///   [`Self::relationships_as_of`].
    /// - Re-asserting the same (source, relation_type, target) is not a
    ///   contradiction and invalidates nothing.
    ///
    /// Returns the number of edges invalidated by this insert.
    ///
    /// # Errors
    /// Returns [`SqliteError`] if the writer transaction fails.
    pub async fn insert_relationship_temporal(
        &self,
        req: &InsertRelationshipRequest<'_>,
    ) -> Result<usize> {
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
                let tx = conn.transaction()?;
                let invalidated = tx.execute(
                    "UPDATE relationships SET t_invalid = ?1 \
                     WHERE source_notion_id = ?2 AND relation_type = ?3 \
                       AND target_notion_id != ?4 AND t_invalid IS NULL AND id != ?5",
                    rusqlite::params![created_at, source_id, rel_type, target_id, id],
                )?;
                tx.execute(
                    "INSERT OR REPLACE INTO relationships \
                     (id, source_notion_id, target_notion_id, relation_type, confidence, \
                      source_note_ids, created_at, description, valid_from, valid_until, weight, \
                      t_valid) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?7)",
                    rusqlite::params![
                        id,
                        source_id,
                        target_id,
                        rel_type,
                        confidence,
                        source_note_ids,
                        created_at,
                        description,
                        valid_from,
                        valid_until,
                        weight
                    ],
                )?;
                tx.commit()?;
                Ok(invalidated)
            })
            .await
            .map_err(SqliteError::TokioRusqlite)
    }

    /// Soft-invalidates an edge by stamping `t_invalid` (RFC3339 UTC).
    ///
    /// The row is never deleted: [`Self::relationships_as_of`] with a timestamp
    /// before `t_invalid` still returns it. First invalidation wins — an
    /// already-closed edge keeps its original stamp. Returns `true` only when
    /// an open edge was closed (`false` for unknown ids or closed edges).
    ///
    /// # Errors
    /// Returns [`SqliteError`] if the writer update fails.
    pub async fn invalidate_relationship(&self, id: &str, t_invalid: &str) -> Result<bool> {
        let id = id.to_string();
        let t_invalid = t_invalid.to_string();

        self.client
            .writer()
            .call(move |conn| {
                let rows = conn.execute(
                    "UPDATE relationships SET t_invalid = ?1 \
                     WHERE id = ?2 AND t_invalid IS NULL",
                    rusqlite::params![t_invalid, id],
                )?;
                Ok(rows > 0)
            })
            .await
            .map_err(SqliteError::TokioRusqlite)
    }

    /// Point-in-time query: every edge valid at `ts` (RFC3339 UTC).
    ///
    /// The validity window is half-open `[t_valid, t_invalid)`: an edge is
    /// returned when `t_valid <= ts` and (`t_invalid IS NULL` or
    /// `t_invalid > ts`). `t_valid = ''` (raw inserts without an explicit
    /// value) means valid since the epoch. Comparison is lexicographic —
    /// callers must pass RFC3339 UTC strings for ordering to hold.
    ///
    /// # Errors
    /// Returns [`SqliteError`] if the query fails.
    pub async fn relationships_as_of(&self, ts: &str) -> Result<Vec<RelationRow>> {
        Ok(sqlx::query_as::<_, RelationRow>(
            "SELECT id, source_notion_id, target_notion_id, relation_type, confidence, \
             source_note_ids, created_at, description, valid_from, valid_until, \
             recorded_at, weight, t_valid, t_invalid \
             FROM relationships \
             WHERE t_valid <= ?1 AND (t_invalid IS NULL OR t_invalid > ?1) \
             ORDER BY t_valid, id",
        )
        .bind(ts)
        .fetch_all(self.client.pool())
        .await?)
    }

    pub async fn load_known_notion_names(&self) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT DISTINCT name FROM notions \
             UNION \
             SELECT DISTINCT alias FROM notion_aliases",
        )
        .fetch_all(self.client.pool())
        .await?;
        Ok(rows.iter().map(|row| row.get::<String, _>(0)).collect())
    }

    pub async fn load_all_entities(&self) -> Result<Vec<NotionRow>> {
        Ok(sqlx::query_as::<_, NotionRow>(
            "SELECT id, name, kind, created_at, domain, last_updated, \
             description, properties, access_count, last_accessed_at, \
             confidence, source, promoted_at \
             FROM notions ORDER BY name",
        )
        .fetch_all(self.client.pool())
        .await?)
    }

    pub async fn load_entities_updated_since(&self, since: &str) -> Result<Vec<NotionRow>> {
        Ok(sqlx::query_as::<_, NotionRow>(
            "SELECT id, name, kind, created_at, domain, last_updated, \
             description, properties, access_count, last_accessed_at, \
             confidence, source, promoted_at \
             FROM notions WHERE last_updated > ?1 ORDER BY name",
        )
        .bind(since)
        .fetch_all(self.client.pool())
        .await?)
    }

    pub async fn resolve_alias(&self, alias: &str) -> Result<Option<String>> {
        let alias = normalize_alias(alias);
        let result = sqlx::query("SELECT canonical_notion_id FROM notion_aliases WHERE alias = ?1")
            .bind(alias)
            .fetch_optional(self.client.pool())
            .await?;
        Ok(result.map(|row| row.get::<String, _>(0)))
    }

    pub async fn load_relationships(&self, notion_id: &str) -> Result<Vec<RelationRow>> {
        Ok(sqlx::query_as::<_, RelationRow>(
            "SELECT id, source_notion_id, target_notion_id, relation_type, confidence, \
             source_note_ids, created_at, description, valid_from, valid_until, \
             recorded_at, weight, t_valid, t_invalid \
             FROM relationships WHERE source_notion_id = ?1",
        )
        .bind(notion_id)
        .fetch_all(self.client.pool())
        .await?)
    }

    pub async fn load_relationships_all(&self, notion_id: &str) -> Result<Vec<RelationRow>> {
        Ok(sqlx::query_as::<_, RelationRow>(
            "SELECT id, source_notion_id, target_notion_id, relation_type, confidence, \
             source_note_ids, created_at, description, valid_from, valid_until, \
             recorded_at, weight, t_valid, t_invalid \
             FROM relationships WHERE source_notion_id = ?1 OR target_notion_id = ?1",
        )
        .bind(notion_id)
        .fetch_all(self.client.pool())
        .await?)
    }

    pub async fn notion_name(&self, notion_id: &str) -> Result<Option<String>> {
        let result = sqlx::query("SELECT name FROM notions WHERE id = ?1")
            .bind(notion_id)
            .fetch_optional(self.client.pool())
            .await?;
        Ok(result.map(|row| row.get::<String, _>(0)))
    }

    pub async fn find_entity_by_name(&self, name: &str) -> Result<Option<NotionRow>> {
        Ok(sqlx::query_as::<_, NotionRow>(
            "SELECT id, name, kind, created_at, domain, last_updated, \
             description, properties, access_count, last_accessed_at, \
             confidence, source, promoted_at \
             FROM notions WHERE name = ?1 COLLATE NOCASE LIMIT 1",
        )
        .bind(name)
        .fetch_optional(self.client.pool())
        .await?)
    }

    pub async fn search_notions_fts(&self, query: &str) -> Result<Vec<NotionRow>> {
        let query = query.trim();
        if query.is_empty() {
            return self.load_all_entities().await;
        }

        let fts_query = format!("{}*", query.replace('"', "''"));

        Ok(sqlx::query_as::<_, NotionRow>(
            "SELECT e.id, e.name, e.kind, e.created_at, e.domain, e.last_updated, \
             e.description, e.properties, e.access_count, e.last_accessed_at, \
             e.confidence, e.source, e.promoted_at \
             FROM notions_fts f \
             JOIN notions e ON e.id = f.notion_id \
             WHERE notions_fts MATCH ?1 \
             ORDER BY rank \
             LIMIT 50",
        )
        .bind(fts_query)
        .fetch_all(self.client.pool())
        .await?)
    }

    pub async fn bfs_search(
        &self,
        notion_name: &str,
        max_depth: u32,
    ) -> Result<Vec<GraphSearchResult>> {
        self.bfs_search_filtered(notion_name, max_depth, "").await
    }

    pub async fn bfs_search_filtered(
        &self,
        notion_name: &str,
        max_depth: u32,
        relation_type_filter: &str,
    ) -> Result<Vec<GraphSearchResult>> {
        if notion_name.trim().is_empty() {
            return Ok(Vec::new());
        }

        let rel_filter = relation_type_filter.to_string();

        let rows = sqlx::query(
            "WITH RECURSIVE \
             edge_set(from_id, to_id, relation_type, direction) AS ( \
                 SELECT source_notion_id, target_notion_id, relation_type, 'outbound' \
                 FROM relationships \
                 WHERE (valid_until IS NULL OR valid_until = '') AND t_invalid IS NULL \
                   AND (?3 = '' OR relation_type = ?3) \
                 UNION ALL \
                 SELECT target_notion_id, source_notion_id, relation_type, 'inbound' \
                 FROM relationships \
                 WHERE (valid_until IS NULL OR valid_until = '') AND t_invalid IS NULL \
                   AND (?3 = '' OR relation_type = ?3) \
             ), \
             bfs(id, name, depth, source_name, relation_type, direction, path) AS ( \
                 SELECT id, name, 0, CAST('' AS TEXT), CAST('' AS TEXT), CAST('' AS TEXT), \
                        ',' || id || ',' \
                 FROM notions WHERE name = ?1 \
                 UNION ALL \
                 SELECT e.id, e.name, b.depth + 1, b.name, edge.relation_type, edge.direction, \
                        b.path || e.id || ',' \
                 FROM bfs b \
                 JOIN edge_set edge ON edge.from_id = b.id \
                 JOIN notions e ON e.id = edge.to_id \
                 WHERE b.depth < ?2 \
                   AND instr(b.path, ',' || e.id || ',') = 0 \
             ) \
             SELECT \
                 b.name as notion, \
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
        .bind(notion_name)
        .bind(max_depth)
        .bind(rel_filter)
        .fetch_all(self.client.pool())
        .await?;

        let results = rows
            .into_iter()
            .map(|row| GraphSearchResult {
                notion: row.get::<String, _>("notion"),
                depth: row.get::<i64, _>("depth") as u32,
                relation: row.get::<String, _>("relation"),
                target: row.get::<String, _>("target"),
                source_entity: row.get::<String, _>("source_entity"),
                direction: row.get::<String, _>("direction"),
            })
            .collect();

        Ok(results)
    }

    pub async fn shortest_paths_all(
        &self,
        notion_name: &str,
        max_depth: u32,
    ) -> Result<Vec<ShortestPathResult>> {
        if notion_name.trim().is_empty() {
            return Ok(Vec::new());
        }

        let rows = sqlx::query(
            "WITH RECURSIVE \
             edge_set(from_id, to_id, weight) AS ( \
                 SELECT source_notion_id, target_notion_id, weight \
                 FROM relationships \
                 WHERE (valid_until IS NULL OR valid_until = '') AND t_invalid IS NULL \
                 UNION ALL \
                 SELECT target_notion_id, source_notion_id, weight \
                 FROM relationships \
                 WHERE (valid_until IS NULL OR valid_until = '') AND t_invalid IS NULL \
             ), \
             walker(id, name, total_weight, depth, path_names, path_ids) AS ( \
                 SELECT id, name, 0.0, 0, name, ',' || id || ',' \
                 FROM notions WHERE name = ?1 \
                 UNION ALL \
                 SELECT e.id, e.name, w.total_weight + edge.weight, w.depth + 1, \
                        w.path_names || ' -> ' || e.name, w.path_ids || e.id || ',' \
                 FROM walker w \
                 JOIN edge_set edge ON edge.from_id = w.id \
                 JOIN notions e ON e.id = edge.to_id \
                 WHERE w.depth < ?2 \
                   AND instr(w.path_ids, ',' || e.id || ',') = 0 \
             ) \
             SELECT pe.name as notion, \
                    pe.total_weight as distance, \
                    pe.depth as depth, \
                    pe.path_names as path \
             FROM walker pe \
             WHERE pe.depth > 0 AND pe.total_weight = ( \
                 SELECT MIN(pe2.total_weight) FROM walker pe2 WHERE pe2.name = pe.name \
             ) \
             ORDER BY pe.total_weight, pe.name",
        )
        .bind(notion_name)
        .bind(max_depth)
        .fetch_all(self.client.pool())
        .await?;

        let results = rows
            .into_iter()
            .map(|row| ShortestPathResult {
                notion: row.get::<String, _>("notion"),
                distance: row.get::<f64, _>("distance"),
                depth: row.get::<i64, _>("depth") as u32,
                path: row.get::<String, _>("path"),
            })
            .collect();

        Ok(results)
    }

    pub async fn shortest_path(
        &self,
        src_name: &str,
        dst_name: &str,
        max_depth: u32,
    ) -> Result<Option<ShortestPathResult>> {
        let all = self.shortest_paths_all(src_name, max_depth).await?;
        Ok(all.into_iter().find(|r| r.notion == dst_name))
    }

    async fn load_graph_core(&self) -> Result<GraphCore> {
        let notion_rows = sqlx::query("SELECT id, name FROM notions ORDER BY name")
            .fetch_all(self.client.pool())
            .await?;

        let n = notion_rows.len();
        let mut name_by_idx = Vec::with_capacity(n);
        let mut name_to_idx = HashMap::new();
        let mut id_to_idx = HashMap::with_capacity(n);
        for (i, row) in notion_rows.iter().enumerate() {
            let id = row.get::<String, _>(0);
            let name = row.get::<String, _>(1);
            id_to_idx.insert(id, i);
            name_to_idx.entry(name.clone()).or_insert(i);
            name_by_idx.push(name);
        }

        let mut out_degree = vec![0usize; n];
        let mut inbound: Vec<Vec<usize>> = vec![Vec::new(); n];

        if n > 0 {
            let edge_rows = sqlx::query(
                "SELECT source_notion_id, target_notion_id FROM relationships \
                 WHERE (valid_until IS NULL OR valid_until = '') AND t_invalid IS NULL",
            )
            .fetch_all(self.client.pool())
            .await?;

            for row in &edge_rows {
                let src = row.get::<String, _>(0);
                let tgt = row.get::<String, _>(1);
                if let (Some(&src_idx), Some(&tgt_idx)) = (id_to_idx.get(&src), id_to_idx.get(&tgt))
                {
                    out_degree[src_idx] += 1;
                    inbound[tgt_idx].push(src_idx);
                }
            }
        }

        Ok(GraphCore {
            name_by_idx,
            name_to_idx,
            id_to_idx,
            out_degree,
            inbound,
        })
    }

    /// Global PageRank over currently-valid edges.
    ///
    /// Equivalent to [`Self::personalized_pagerank`] with a uniform teleport
    /// vector and `restart = 1.0 - damping`; scores sum to ~1.0.
    ///
    /// # Errors
    /// Returns [`SqliteError`] if the graph cannot be loaded.
    pub async fn pagerank(&self, iterations: usize, damping: f64) -> Result<Vec<PageRankResult>> {
        let core = self.load_graph_core().await?;
        let n = core.name_by_idx.len();
        if n == 0 {
            return Ok(Vec::new());
        }
        Ok(run_pagerank_core(
            &core,
            vec![1.0 / n as f64; n],
            iterations,
            damping,
            1.0 - damping,
        ))
    }

    /// Personalized PageRank (HippoRAG pattern): seeded retrieval ranking over
    /// the directed entity graph of currently-valid edges (`t_invalid IS NULL`).
    ///
    /// Each iteration the random walk teleports back to the seed distribution
    /// with probability `restart` instead of jumping uniformly:
    ///
    /// ```text
    /// pr[i] ← restart·teleport[i]
    ///       + damping·(dangling_sum·teleport[i] + Σ_{src→i} pr[src]/out[src])
    /// ```
    ///
    /// with `pr` initialized to `teleport` (uniform over resolved seeds). The
    /// canonical parameterization is `restart = 1.0 - damping` (e.g. 0.15 /
    /// 0.85); other combinations still rank but do not conserve total mass.
    ///
    /// Seeds match notion *names* first, then ids; unrecognized seeds are
    /// ignored. Returns every entity sorted by descending personalized score —
    /// entities unreachable from the seeds score ≈ 0. Returns an empty vector
    /// when the graph is empty or no seed resolves.
    ///
    /// # Errors
    /// Returns [`SqliteError`] if the graph cannot be loaded.
    pub async fn personalized_pagerank(
        &self,
        seeds: &[String],
        iterations: usize,
        damping: f64,
        restart: f64,
    ) -> Result<Vec<PageRankResult>> {
        let core = self.load_graph_core().await?;
        let n = core.name_by_idx.len();
        if n == 0 {
            return Ok(Vec::new());
        }

        let seed_idx: HashSet<usize> = seeds
            .iter()
            .filter_map(|seed| {
                core.name_to_idx
                    .get(seed)
                    .or_else(|| core.id_to_idx.get(seed))
                    .copied()
            })
            .collect();
        if seed_idx.is_empty() {
            return Ok(Vec::new());
        }

        let share = 1.0 / seed_idx.len() as f64;
        let mut teleport = vec![0.0; n];
        for &i in &seed_idx {
            teleport[i] = share;
        }

        Ok(run_pagerank_core(
            &core, teleport, iterations, damping, restart,
        ))
    }

    pub async fn connected_components(&self) -> Result<Vec<ComponentResult>> {
        let notion_rows = sqlx::query("SELECT id, name FROM notions ORDER BY name")
            .fetch_all(self.client.pool())
            .await?;

        let notions: Vec<(String, String)> = notion_rows
            .iter()
            .map(|r| (r.get::<String, _>(0), r.get::<String, _>(1)))
            .collect();

        let n = notions.len();
        if n == 0 {
            return Ok(Vec::new());
        }

        let edge_rows = sqlx::query(
            "SELECT source_notion_id, target_notion_id FROM relationships \
             WHERE (valid_until IS NULL OR valid_until = '') AND t_invalid IS NULL",
        )
        .fetch_all(self.client.pool())
        .await?;

        let id_to_idx: HashMap<String, usize> = notions
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (id.clone(), i))
            .collect();

        let mut adj: Vec<HashSet<usize>> = vec![HashSet::new(); n];
        for row in &edge_rows {
            let src = row.get::<String, _>(0);
            let tgt = row.get::<String, _>(1);
            if let (Some(&s), Some(&t)) = (id_to_idx.get(&src), id_to_idx.get(&tgt)) {
                adj[s].insert(t);
                adj[t].insert(s);
            }
        }

        let mut component_id = vec![-1i64; n];
        let mut component_sizes: HashMap<i64, i64> = HashMap::new();
        let mut current_component = 0i64;

        for start in 0..n {
            if component_id[start] != -1 {
                continue;
            }

            let mut queue = vec![start];
            let mut visited = HashSet::new();
            visited.insert(start);
            component_id[start] = current_component;

            while let Some(node) = queue.pop() {
                for &neighbor in &adj[node] {
                    if !visited.contains(&neighbor) {
                        visited.insert(neighbor);
                        component_id[neighbor] = current_component;
                        queue.push(neighbor);
                    }
                }
            }

            let size = visited.len() as i64;
            component_sizes.insert(current_component, size);

            current_component += 1;
        }

        let results = notions
            .iter()
            .enumerate()
            .map(|(i, (_, name))| ComponentResult {
                notion: name.clone(),
                component_id: component_id[i],
                component_size: component_sizes[&component_id[i]],
            })
            .collect();

        Ok(results)
    }

    /// Deterministic Louvain communities over the undirected weighted projection
    /// of currently-open relationship edges (T141).
    ///
    /// Reuses [`Self::load_graph_core`] — the same valid-edge snapshot shared by
    /// [`Self::pagerank`] and [`Self::personalized_pagerank`] — so the
    /// bi-temporal semantics (`t_invalid IS NULL`, `valid_until` empty) cannot
    /// drift between traversals. Edge weight = number of edges in either
    /// direction between the pair. Isolated notions (no open edges) are not part
    /// of the projection and do not appear in the output.
    ///
    /// `resolution` (γ) trades granularity for cohesion; the default is 1.0.
    ///
    /// # Errors
    /// Returns [`SqliteError`] if the graph cannot be loaded.
    pub async fn compute_communities(&self, resolution: f64) -> Result<Vec<Community>> {
        let core = self.load_graph_core().await?;

        // Undirected weighted projection: each open edge contributes 1 to the
        // weight between its endpoints, in both directions.
        let mut adjacency: HashMap<String, HashMap<String, f64>> = HashMap::new();
        for (tgt_idx, srcs) in core.inbound.iter().enumerate() {
            for &src_idx in srcs {
                let sn = &core.name_by_idx[src_idx];
                let tn = &core.name_by_idx[tgt_idx];
                if sn == tn {
                    continue;
                }
                *adjacency
                    .entry(sn.clone())
                    .or_default()
                    .entry(tn.clone())
                    .or_insert(0.0) += 1.0;
                *adjacency
                    .entry(tn.clone())
                    .or_default()
                    .entry(sn.clone())
                    .or_insert(0.0) += 1.0;
            }
        }

        Ok(crate::communities::louvain_communities(
            &adjacency, resolution,
        ))
    }

    /// Persist a community run, replacing any previous rows for the same
    /// (algorithm, resolution) so the tables cannot grow monotonically with each
    /// cycle (T141).
    ///
    /// The delete and inserts run in one transaction; members are written with
    /// their node weight (weighted degree in the projected graph).
    ///
    /// # Errors
    /// Returns [`SqliteError`] if the transaction fails.
    pub async fn replace_communities(
        &self,
        algorithm: &str,
        resolution: f64,
        communities: &[Community],
        computed_at: &str,
    ) -> Result<()> {
        let algorithm = algorithm.to_string();
        let computed_at = computed_at.to_string();
        let communities: Vec<Community> = communities.to_vec();

        self.client
            .writer()
            .call(move |conn| {
                let tx = conn.transaction()?;
                tx.execute(
                    "DELETE FROM notion_community_members WHERE community_id IN \
                     (SELECT id FROM notion_communities WHERE algorithm = ?1 AND resolution = ?2)",
                    rusqlite::params![algorithm, resolution],
                )?;
                tx.execute(
                    "DELETE FROM notion_communities WHERE algorithm = ?1 AND resolution = ?2",
                    rusqlite::params![algorithm, resolution],
                )?;
                for c in &communities {
                    // Scope the id by algorithm+resolution: the members table
                    // is keyed by community_id alone, so ids must be globally
                    // unique across coexisting runs (resolution is data).
                    let id = format!("{algorithm}:{resolution}:{}", c.id);
                    tx.execute(
                        "INSERT INTO notion_communities \
                         (id, algorithm, resolution, computed_at, label, size) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![
                            id,
                            algorithm,
                            resolution,
                            computed_at,
                            c.label,
                            c.members.len() as i64
                        ],
                    )?;
                    for m in &c.members {
                        tx.execute(
                            "INSERT INTO notion_community_members \
                             (community_id, entity_name, weight) VALUES (?1, ?2, ?3)",
                            rusqlite::params![id, m.entity_name, m.weight],
                        )?;
                    }
                }
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)
    }

    /// Load all persisted communities with their members, ordered by
    /// (algorithm, resolution, id) then member name (T141).
    ///
    /// # Errors
    /// Returns [`SqliteError`] if the query fails.
    pub async fn load_communities(&self) -> Result<Vec<Community>> {
        let pool = self.client.pool();
        let rows = sqlx::query(
            "SELECT c.id, c.label, m.entity_name, m.weight \
             FROM notion_communities c \
             LEFT JOIN notion_community_members m ON m.community_id = c.id \
             ORDER BY c.algorithm, c.resolution, c.id, m.entity_name",
        )
        .fetch_all(pool)
        .await?;

        let mut communities: Vec<Community> = Vec::new();
        let mut current: Option<Community> = None;
        for row in &rows {
            let id: String = row.get("id");
            let label: String = row.get("label");
            let entity_name: Option<String> = row.get("entity_name");
            let weight: Option<f64> = row.get("weight");

            match &mut current {
                Some(c) if c.id == id => {
                    if let (Some(name), Some(w)) = (entity_name, weight) {
                        c.members.push(CommunityMember {
                            entity_name: name,
                            weight: w,
                        });
                    }
                }
                _ => {
                    if let Some(c) = current.take() {
                        communities.push(c);
                    }
                    let mut members = Vec::new();
                    if let (Some(name), Some(w)) = (entity_name, weight) {
                        members.push(CommunityMember {
                            entity_name: name,
                            weight: w,
                        });
                    }
                    current = Some(Community { id, label, members });
                }
            }
        }
        if let Some(c) = current.take() {
            communities.push(c);
        }
        Ok(communities)
    }

    pub async fn apply_confidence_decay(&self, half_life_days: f64) -> Result<usize> {
        let rows = sqlx::query(
            "SELECT id, confidence, COALESCE(last_accessed_at, created_at) as ref_date \
             FROM notions WHERE confidence > 0.0",
        )
        .fetch_all(self.client.pool())
        .await?;

        let now = chrono::Utc::now();
        let mut count = 0usize;

        for row in &rows {
            let id: String = row.get(0);
            let confidence: f64 = row.get(1);
            let ref_date_str: String = row.get(2);

            let days = chrono::DateTime::parse_from_rfc3339(&ref_date_str)
                .map(|dt| {
                    let dt_utc = dt.with_timezone(&chrono::Utc);
                    ((now - dt_utc).num_milliseconds() as f64 / 86_400_000.0).max(0.0)
                })
                .unwrap_or(0.0);

            let decay_factor = 0.5_f64.powf(days / half_life_days);
            let new_confidence = (confidence * decay_factor).max(0.01);

            if (new_confidence - confidence).abs() > 0.001 {
                self.update_entity_confidence(&id, new_confidence).await?;
                count += 1;
            }
        }

        Ok(count)
    }

    pub async fn auto_promote_entities(&self, access_threshold: i64) -> Result<usize> {
        let now = chrono::Utc::now().to_rfc3339();
        let threshold = access_threshold;

        self.client
            .writer()
            .call(move |conn| {
                let rows = conn.execute(
                    "UPDATE notions \
                     SET promoted_at = ?1, confidence = MAX(confidence, 0.8) \
                     WHERE access_count >= ?2 AND promoted_at IS NULL",
                    rusqlite::params![now, threshold],
                )?;
                Ok(rows)
            })
            .await
            .map_err(SqliteError::TokioRusqlite)
    }

    pub async fn compute_importance(
        &self,
        iterations: usize,
        damping: f64,
    ) -> Result<Vec<PageRankResult>> {
        self.pagerank(iterations, damping).await
    }

    /// Record the last stage a note reached, keyed by its identity
    /// `(file_path, content_hash)` (FR-011). Upserts, so a note re-entering the
    /// pipeline with the same content refreshes its stage rather than
    /// duplicating a row.
    pub async fn record_note_stage(
        &self,
        file_path: &str,
        content_hash: &str,
        stage: &str,
        now: &str,
    ) -> Result<()> {
        let pool = self.client.pool();
        sqlx::query(
            "INSERT INTO note_stages (file_path, content_hash, last_completed_stage, stage_updated_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(file_path, content_hash) \
             DO UPDATE SET last_completed_stage = ?3, stage_updated_at = ?4",
        )
        .bind(file_path)
        .bind(content_hash)
        .bind(stage)
        .bind(now)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Every recorded note stage for the given terminal stage, for re-deriving
    /// cycle state from the durable projection instead of trusting in-memory
    /// bookkeeping (FR-011). Filtered by `last_completed_stage` so the
    /// per-cycle lookup never scans rows for stages the pipeline does not use
    /// (T162).
    pub async fn load_note_stages(&self, stage: &str) -> Result<Vec<NoteStageRow>> {
        let pool = self.client.pool();
        let rows = sqlx::query(
            "SELECT file_path, content_hash, last_completed_stage FROM note_stages \
             WHERE last_completed_stage = ?1",
        )
        .bind(stage)
        .fetch_all(pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| NoteStageRow {
                file_path: row.get("file_path"),
                content_hash: row.get("content_hash"),
                last_completed_stage: row.get("last_completed_stage"),
            })
            .collect())
    }

    /// Prune `note_stages` rows whose recorded path no longer exists on disk,
    /// capped by age so the table cannot grow forever (T162).
    ///
    /// A row is removed only when BOTH conditions hold: its `stage_updated_at`
    /// is older than `max_age_days` AND the `file_path` no longer exists in
    /// the vault (the note was archived and its inbox copy removed). Rows for
    /// paths that still exist — or rows younger than the cap — are kept, so a
    /// temporarily absent path is never pruned.
    pub async fn prune_note_stages(&self, max_age_days: i64) -> Result<usize> {
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(max_age_days)).to_rfc3339();
        let pool = self.client.pool();
        let rows = sqlx::query("SELECT file_path FROM note_stages WHERE stage_updated_at < ?1")
            .bind(&cutoff)
            .fetch_all(pool)
            .await?;
        let mut to_delete: Vec<String> = Vec::new();
        for row in rows {
            let file_path: String = row.get("file_path");
            if !std::path::Path::new(&file_path).exists() {
                to_delete.push(file_path);
            }
        }
        if to_delete.is_empty() {
            return Ok(0);
        }
        // The candidate set is bounded by the age cap, so per-path deletes are
        // cheap and keep the SQL static (no dynamic IN clause).
        for path in &to_delete {
            sqlx::query("DELETE FROM note_stages WHERE file_path = ?1")
                .bind(path)
                .execute(pool)
                .await?;
        }
        Ok(to_delete.len())
    }
}

impl crate::traits::notions::NotionsRepository for NotionsRepo<'_> {
    async fn insert_entity(&self, id: &str, name: &str, kind: &str, now: &str) -> Result<()> {
        NotionsRepo::insert_entity(self, id, name, kind, now).await
    }

    async fn insert_entity_with(
        &self,
        id: &str,
        name: &str,
        kind: &str,
        now: &str,
        description: &str,
        source: &str,
        confidence: f64,
    ) -> Result<()> {
        NotionsRepo::insert_entity_with(self, id, name, kind, now, description, source, confidence)
            .await
    }

    async fn upsert_entity(
        &self,
        id: &str,
        name: &str,
        kind: &str,
        created_at: &str,
        last_updated: &str,
    ) -> Result<()> {
        NotionsRepo::upsert_entity(self, id, name, kind, created_at, last_updated).await
    }

    async fn upsert_entity_with(
        &self,
        id: &str,
        name: &str,
        kind: &str,
        created_at: &str,
        last_updated: &str,
        description: &str,
        source: &str,
        confidence: f64,
    ) -> Result<()> {
        NotionsRepo::upsert_entity_with(
            self,
            id,
            name,
            kind,
            created_at,
            last_updated,
            description,
            source,
            confidence,
        )
        .await
    }

    async fn update_entity_timestamp(&self, notion_id: &str, last_updated: &str) -> Result<()> {
        NotionsRepo::update_entity_timestamp(self, notion_id, last_updated).await
    }

    async fn update_entity_access(&self, notion_id: &str) -> Result<()> {
        NotionsRepo::update_entity_access(self, notion_id).await
    }

    async fn update_entity_confidence(&self, notion_id: &str, confidence: f64) -> Result<()> {
        NotionsRepo::update_entity_confidence(self, notion_id, confidence).await
    }

    async fn promote_entity(&self, notion_id: &str) -> Result<()> {
        NotionsRepo::promote_entity(self, notion_id).await
    }

    async fn insert_alias(&self, alias: &str, canonical_notion_id: &str) -> Result<()> {
        NotionsRepo::insert_alias(self, alias, canonical_notion_id).await
    }

    async fn load_aliases_for_entity(&self, notion_id: &str) -> Result<Vec<String>> {
        NotionsRepo::load_aliases_for_entity(self, notion_id).await
    }

    async fn insert_relationship(&self, req: &InsertRelationshipRequest<'_>) -> Result<()> {
        NotionsRepo::insert_relationship(self, req).await
    }

    async fn insert_relationship_temporal(
        &self,
        req: &InsertRelationshipRequest<'_>,
    ) -> Result<usize> {
        NotionsRepo::insert_relationship_temporal(self, req).await
    }

    async fn invalidate_relationship(&self, id: &str, t_invalid: &str) -> Result<bool> {
        NotionsRepo::invalidate_relationship(self, id, t_invalid).await
    }

    async fn relationships_as_of(&self, ts: &str) -> Result<Vec<RelationRow>> {
        NotionsRepo::relationships_as_of(self, ts).await
    }

    async fn load_known_notion_names(&self) -> Result<Vec<String>> {
        NotionsRepo::load_known_notion_names(self).await
    }

    async fn load_all_entities(&self) -> Result<Vec<NotionRow>> {
        NotionsRepo::load_all_entities(self).await
    }

    async fn load_entities_updated_since(&self, since: &str) -> Result<Vec<NotionRow>> {
        NotionsRepo::load_entities_updated_since(self, since).await
    }

    async fn resolve_alias(&self, alias: &str) -> Result<Option<String>> {
        NotionsRepo::resolve_alias(self, alias).await
    }

    async fn load_relationships(&self, notion_id: &str) -> Result<Vec<RelationRow>> {
        NotionsRepo::load_relationships(self, notion_id).await
    }

    async fn load_relationships_all(&self, notion_id: &str) -> Result<Vec<RelationRow>> {
        NotionsRepo::load_relationships_all(self, notion_id).await
    }

    async fn notion_name(&self, notion_id: &str) -> Result<Option<String>> {
        NotionsRepo::notion_name(self, notion_id).await
    }

    async fn find_entity_by_name(&self, name: &str) -> Result<Option<NotionRow>> {
        NotionsRepo::find_entity_by_name(self, name).await
    }

    async fn search_notions_fts(&self, query: &str) -> Result<Vec<NotionRow>> {
        NotionsRepo::search_notions_fts(self, query).await
    }

    async fn bfs_search(
        &self,
        notion_name: &str,
        max_depth: u32,
    ) -> Result<Vec<GraphSearchResult>> {
        NotionsRepo::bfs_search(self, notion_name, max_depth).await
    }

    async fn bfs_search_filtered(
        &self,
        notion_name: &str,
        max_depth: u32,
        relation_type_filter: &str,
    ) -> Result<Vec<GraphSearchResult>> {
        NotionsRepo::bfs_search_filtered(self, notion_name, max_depth, relation_type_filter).await
    }

    async fn shortest_paths_all(
        &self,
        notion_name: &str,
        max_depth: u32,
    ) -> Result<Vec<ShortestPathResult>> {
        NotionsRepo::shortest_paths_all(self, notion_name, max_depth).await
    }

    async fn shortest_path(
        &self,
        src_name: &str,
        dst_name: &str,
        max_depth: u32,
    ) -> Result<Option<ShortestPathResult>> {
        NotionsRepo::shortest_path(self, src_name, dst_name, max_depth).await
    }

    async fn pagerank(&self, iterations: usize, damping: f64) -> Result<Vec<PageRankResult>> {
        NotionsRepo::pagerank(self, iterations, damping).await
    }

    async fn personalized_pagerank(
        &self,
        seeds: &[String],
        iterations: usize,
        damping: f64,
        restart: f64,
    ) -> Result<Vec<PageRankResult>> {
        NotionsRepo::personalized_pagerank(self, seeds, iterations, damping, restart).await
    }

    async fn connected_components(&self) -> Result<Vec<ComponentResult>> {
        NotionsRepo::connected_components(self).await
    }

    async fn apply_confidence_decay(&self, half_life_days: f64) -> Result<usize> {
        NotionsRepo::apply_confidence_decay(self, half_life_days).await
    }

    async fn auto_promote_entities(&self, access_threshold: i64) -> Result<usize> {
        NotionsRepo::auto_promote_entities(self, access_threshold).await
    }

    async fn compute_importance(
        &self,
        iterations: usize,
        damping: f64,
    ) -> Result<Vec<PageRankResult>> {
        NotionsRepo::compute_importance(self, iterations, damping).await
    }
}
