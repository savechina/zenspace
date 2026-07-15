use std::collections::{HashMap, HashSet};

use sqlx::Row;

use crate::client::{Result, SqliteClient, SqliteError};
use crate::types::{
    ComponentResult, NotionRow, GraphSearchResult, InsertRelationshipRequest, PageRankResult,
    RelationshipRow, ShortestPathResult,
};

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

pub struct NotionsRepo<'a> {
    client: &'a SqliteClient,
}

impl<'a> NotionsRepo<'a> {
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
    }

    pub async fn insert_entity(
        &self,
        id: &str,
        name: &str,
        kind: &str,
        now: &str,
    ) -> Result<()> {
        self.insert_entity_with(id, name, kind, now, "", "manual", 0.5)
            .await
    }

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

    pub async fn update_entity_confidence(
        &self,
        notion_id: &str,
        confidence: f64,
    ) -> Result<()> {
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

    pub async fn load_relationships(&self, notion_id: &str) -> Result<Vec<RelationshipRow>> {
        Ok(sqlx::query_as::<_, RelationshipRow>(
            "SELECT id, source_notion_id, target_notion_id, relation_type, confidence, \
             source_note_ids, created_at, description, valid_from, valid_until, \
             recorded_at, weight \
             FROM relationships WHERE source_notion_id = ?1",
        )
        .bind(notion_id)
        .fetch_all(self.client.pool())
        .await?)
    }

    pub async fn load_relationships_all(&self, notion_id: &str) -> Result<Vec<RelationshipRow>> {
        Ok(sqlx::query_as::<_, RelationshipRow>(
            "SELECT id, source_notion_id, target_notion_id, relation_type, confidence, \
             source_note_ids, created_at, description, valid_from, valid_until, \
             recorded_at, weight \
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
                 WHERE (valid_until IS NULL OR valid_until = '') \
                   AND (?3 = '' OR relation_type = ?3) \
                 UNION ALL \
                 SELECT target_notion_id, source_notion_id, relation_type, 'inbound' \
                 FROM relationships \
                 WHERE (valid_until IS NULL OR valid_until = '') \
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
                 FROM relationships WHERE (valid_until IS NULL OR valid_until = '') \
                 UNION ALL \
                 SELECT target_notion_id, source_notion_id, weight \
                 FROM relationships WHERE (valid_until IS NULL OR valid_until = '') \
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

    pub async fn pagerank(
        &self,
        iterations: usize,
        damping: f64,
    ) -> Result<Vec<PageRankResult>> {
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
             WHERE valid_until IS NULL OR valid_until = ''",
        )
        .fetch_all(self.client.pool())
        .await?;

        let edges: Vec<(String, String)> = edge_rows
            .iter()
            .map(|r| (r.get::<String, _>(0), r.get::<String, _>(1)))
            .collect();

        let id_to_idx: HashMap<String, usize> = notions
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (id.clone(), i))
            .collect();

        let name_by_idx: Vec<String> = notions.iter().map(|(_, name)| name.clone()).collect();

        let mut out_degree = vec![0usize; n];
        let mut inbound: Vec<Vec<usize>> = vec![Vec::new(); n];

        for (src, tgt) in &edges {
            if let (Some(&src_idx), Some(&tgt_idx)) =
                (id_to_idx.get(src), id_to_idx.get(tgt))
            {
                out_degree[src_idx] += 1;
                inbound[tgt_idx].push(src_idx);
            }
        }

        let n_f64 = n as f64;
        let mut pr = vec![1.0 / n_f64; n];

        for _ in 0..iterations {
            let dangling_sum: f64 = pr
                .iter()
                .enumerate()
                .filter(|(i, _)| out_degree[*i] == 0)
                .map(|(_, &score)| score)
                .sum();
            let dangling_share = damping * dangling_sum / n_f64;

            let mut new_pr = vec![(1.0 - damping) / n_f64 + dangling_share; n];

            for i in 0..n {
                for &src_idx in &inbound[i] {
                    if out_degree[src_idx] > 0 {
                        new_pr[i] += damping * pr[src_idx] / out_degree[src_idx] as f64;
                    }
                }
            }

            pr = new_pr;
        }

        let mut results: Vec<PageRankResult> = pr
            .iter()
            .enumerate()
            .map(|(i, &score)| PageRankResult {
                notion: name_by_idx[i].clone(),
                score,
            })
            .collect();
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(results)
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
             WHERE valid_until IS NULL OR valid_until = ''",
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
}
