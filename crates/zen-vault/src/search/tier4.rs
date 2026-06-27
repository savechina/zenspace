use std::collections::{HashSet, VecDeque};
use std::path::Path;

use anyhow::Result;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use tracing::debug;

use crate::tools::{ZenTool, ZenToolError, ZenToolResult, args_schema_entity, result_schema_array};

pub struct GraphResult {
    pub entity: String,
    pub depth: u32,
    pub relation: String,
    pub target: String,
}

/// Tier 4 search: entity graph traversal with BFS.
#[derive(Debug)]
pub struct Tier4Search;

impl Tier4Search {
    fn ensure_graph(db_path: &Path) -> Result<rusqlite::Connection> {
        if !db_path.exists() {
            anyhow::bail!(
                "Graph database not found at {}. Initialize with init_graph_schema() first.",
                db_path.display()
            );
        }
        let conn = rusqlite::Connection::open(db_path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS entities (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                entity_type TEXT NOT NULL,
                aliases TEXT,
                first_seen TEXT NOT NULL,
                last_updated TEXT NOT NULL,
                domain TEXT,
                UNIQUE(name, entity_type)
            );
            CREATE INDEX IF NOT EXISTS idx_entities_name ON entities(name);
            CREATE TABLE IF NOT EXISTS relationships (
                id TEXT PRIMARY KEY,
                source_entity_id TEXT NOT NULL,
                target_entity_id TEXT NOT NULL,
                relation_type TEXT NOT NULL,
                confidence REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
                source_note_ids TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (source_entity_id) REFERENCES entities(id),
                FOREIGN KEY (target_entity_id) REFERENCES entities(id),
                CHECK(source_entity_id != target_entity_id)
            );
            CREATE INDEX IF NOT EXISTS idx_rel_source ON relationships(source_entity_id);
            CREATE INDEX IF NOT EXISTS idx_rel_target ON relationships(target_entity_id);
            CREATE TABLE IF NOT EXISTS entity_aliases (
                alias TEXT NOT NULL,
                canonical_entity_id TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (alias, canonical_entity_id),
                FOREIGN KEY (canonical_entity_id) REFERENCES entities(id)
            );
            CREATE INDEX IF NOT EXISTS idx_aliases_lookup ON entity_aliases(alias);
            CREATE TABLE IF NOT EXISTS goal_nodes (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                controllability REAL NOT NULL DEFAULT 0.5 CHECK(controllability >= 0.0 AND controllability <= 1.0),
                core_pursuit TEXT,
                deadline TEXT,
                created_at TEXT NOT NULL,
                last_updated TEXT NOT NULL,
                FOREIGN KEY (id) REFERENCES entities(id)
            );
            CREATE INDEX IF NOT EXISTS idx_goal_nodes_name ON goal_nodes(name);
            CREATE TABLE IF NOT EXISTS path_nodes (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                serves_goal_id TEXT,
                is_default INTEGER NOT NULL DEFAULT 0,
                crowdedness REAL NOT NULL DEFAULT 0.5 CHECK(crowdedness >= 0.0 AND crowdedness <= 1.0),
                alternatives TEXT,
                created_at TEXT NOT NULL,
                last_updated TEXT NOT NULL,
                FOREIGN KEY (id) REFERENCES entities(id),
                FOREIGN KEY (serves_goal_id) REFERENCES goal_nodes(id)
            );
            CREATE INDEX IF NOT EXISTS idx_path_nodes_name ON path_nodes(name);
            CREATE INDEX IF NOT EXISTS idx_path_nodes_goal ON path_nodes(serves_goal_id);
            CREATE TABLE IF NOT EXISTS belief_nodes (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                proposition TEXT NOT NULL,
                prior REAL NOT NULL DEFAULT 0.5 CHECK(prior >= 0.01 AND prior <= 0.99),
                posterior REAL NOT NULL DEFAULT 0.5 CHECK(posterior >= 0.01 AND posterior <= 0.99),
                evidence_count INTEGER NOT NULL DEFAULT 0,
                last_updated TEXT NOT NULL,
                FOREIGN KEY (id) REFERENCES entities(id)
            );
            CREATE INDEX IF NOT EXISTS idx_belief_nodes_name ON belief_nodes(name);
            CREATE INDEX IF NOT EXISTS idx_belief_nodes_posterior ON belief_nodes(posterior);",
        )?;
        Ok(conn)
    }

    pub fn search(
        &self,
        entity_name: &str,
        db_path: &Path,
        max_depth: u32,
    ) -> Result<Vec<GraphResult>> {
        if entity_name.trim().is_empty() || !db_path.exists() {
            return Ok(Vec::new());
        }

        let conn = Self::ensure_graph(db_path)?;

        let entity_id: Option<String> = conn
            .query_row(
                "SELECT id FROM entities WHERE name = ?1",
                rusqlite::params![entity_name],
                |row| row.get(0),
            )
            .optional()?;

        let Some(start_id) = entity_id else {
            debug!("Tier4Search: entity '{}' not found", entity_name);
            return Ok(Vec::new());
        };

        let mut visited = HashSet::new();
        let mut queue: VecDeque<(String, u32)> = VecDeque::new();
        queue.push_back((start_id.clone(), 0));
        visited.insert(start_id.clone());

        let mut entity_relations: Vec<(String, String, String, u32)> = Vec::new();

        while let Some((current_id, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }

            let current_name: String = conn.query_row(
                "SELECT name FROM entities WHERE id = ?1",
                rusqlite::params![current_id],
                |row| row.get(0),
            )?;

            let mut stmt = conn.prepare(
                "SELECT r.relation_type, e.name, e.id
                 FROM relationships r
                 JOIN entities e ON e.id = r.target_entity_id
                 WHERE r.source_entity_id = ?1",
            )?;

            let neighbors: Vec<(String, String, String)> = stmt
                .query_map(rusqlite::params![current_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .filter_map(|r| r.ok())
                .collect();

            for (relation_type, target_name, target_id) in &neighbors {
                entity_relations.push((
                    current_name.clone(),
                    relation_type.clone(),
                    target_name.clone(),
                    depth + 1,
                ));

                if visited.insert(target_id.clone()) {
                    queue.push_back((target_id.clone(), depth + 1));
                }
            }
        }

        let mut graph_results: Vec<GraphResult> = Vec::new();
        let mut seen = HashSet::new();

        for (entity_name, _relation, _target, depth) in &entity_relations {
            if seen.insert(entity_name.clone()) {
                let depth_val = *depth;
                let mut relations: Vec<String> = entity_relations
                    .iter()
                    .filter(|(en, _, _, _)| en == entity_name)
                    .map(|(_, rel, _, _)| rel.clone())
                    .collect();
                relations.sort();
                relations.dedup();
                graph_results.push(GraphResult {
                    entity: entity_name.clone(),
                    depth: depth_val,
                    relation: relations.join(", "),
                    target: entity_name.clone(),
                });
            }
        }

        debug!(
            "Tier4Search: found {} entities for '{}' (depth={})",
            graph_results.len(),
            entity_name,
            max_depth
        );
        Ok(graph_results)
    }

    // TODO: route through EntityService::upsert_entity for alias normalization
    pub fn insert_entity(
        &self,
        db_path: &Path,
        id: &str,
        name: &str,
        entity_type: &str,
    ) -> Result<()> {
        if !db_path.exists() {
            anyhow::bail!(
                "Graph database not found at {}. Initialize with init_graph_schema() first.",
                db_path.display()
            );
        }

        let conn = Self::ensure_graph(db_path)?;
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "INSERT OR REPLACE INTO entities (id, name, entity_type, first_seen, last_updated) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, name, entity_type, now, now],
        )?;

        Ok(())
    }

    pub fn insert_relationship(
        &self,
        db_path: &Path,
        id: &str,
        source_id: &str,
        target_id: &str,
        relation_type: &str,
        confidence: f64,
    ) -> Result<()> {
        if !db_path.exists() {
            anyhow::bail!(
                "Graph database not found at {}. Initialize with init_graph_schema() first.",
                db_path.display()
            );
        }

        let conn = Self::ensure_graph(db_path)?;
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "INSERT OR REPLACE INTO relationships (id, source_entity_id, target_entity_id, relation_type, confidence, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, source_id, target_id, relation_type, confidence, now],
        )?;

        Ok(())
    }
}

impl ZenTool for Tier4Search {
    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "tier4_search".to_string(),
            description: "Entity graph traversal using BFS from a starting entity.".to_string(),
            args_schema: args_schema_entity(),
            result_schema: result_schema_array(),
        }
    }

    async fn invoke(&self, args: Value) -> ZenToolResult {
        let entity_name = args
            .get("entity_name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ZenToolError::InvalidArgs("missing required field: entity_name".to_string())
            })?;
        let max_depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(3) as usize;
        let db_path = args
            .get("db_path")
            .and_then(Value::as_str)
            .unwrap_or("graph.db");

        let results = self
            .search(entity_name, Path::new(db_path), max_depth as u32)
            .map_err(|e| ZenToolError::ExecutionFailed(e.to_string()))?;

        let formatted: Vec<Value> = results
            .into_iter()
            .map(|r| {
                json!({
                    "entity": r.entity,
                    "depth": r.depth,
                    "relation": r.relation,
                    "target": r.target,
                })
            })
            .collect();

        Ok(json!({ "entities": formatted }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_tier4_empty_query_returns_empty() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let tier4 = Tier4Search;
        let results = tier4.search("test", &db_path, 3).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_tier4_insert_and_search_graph() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let _conn = rusqlite::Connection::open(&db_path).unwrap();
        let tier4 = Tier4Search;

        tier4
            .insert_entity(&db_path, "e1", "Alice", "person")
            .unwrap();
        tier4
            .insert_entity(&db_path, "e2", "Bob", "person")
            .unwrap();
        tier4
            .insert_entity(&db_path, "e3", "Charly", "person")
            .unwrap();

        tier4
            .insert_relationship(&db_path, "r1", "e1", "e2", "knows", 0.9)
            .unwrap();
        tier4
            .insert_relationship(&db_path, "r2", "e2", "e3", "knows", 0.8)
            .unwrap();

        let results = tier4.search("Alice", &db_path, 2).unwrap();
        assert!(results.len() >= 2);
    }

    #[test]
    fn test_tier4_bfs_depth_limit() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let _conn = rusqlite::Connection::open(&db_path).unwrap();
        let tier4 = Tier4Search;

        for i in 0..5 {
            tier4
                .insert_entity(&db_path, &format!("e{i}"), &format!("N{i}"), "node")
                .unwrap();
        }
        for i in 0..4 {
            tier4
                .insert_relationship(
                    &db_path,
                    &format!("r{i}"),
                    &format!("e{i}"),
                    &format!("e{}", i + 1),
                    "next",
                    1.0,
                )
                .unwrap();
        }

        let results_depth1 = tier4.search("N0", &db_path, 1).unwrap();
        let results_depth3 = tier4.search("N0", &db_path, 3).unwrap();
        assert!(results_depth3.len() >= results_depth1.len());
    }

    #[test]
    fn test_tier4_missing_db_fails() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("nonexistent.db");
        let tier4 = Tier4Search;

        let result = tier4.insert_entity(&db_path, "x", "X", "type");
        assert!(result.is_err());
    }

    #[test]
    fn test_tier4_tool_schema() {
        let tier4 = Tier4Search;
        let schema = tier4.schema();
        assert_eq!(schema.name, "tier4_search");
        assert!(schema.description.contains("BFS"));
    }
}
