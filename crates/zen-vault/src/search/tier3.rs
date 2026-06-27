use std::path::Path;

use anyhow::Result;
use rig_core::Embed;
use rig_sqlite::{Column, ColumnValue, SqliteVectorStoreTable};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::SearchResult;

// ---------------------------------------------------------------------------
// Tier 3 search: sqlite-vec KNN cosine similarity + rig-sqlite integration
// ---------------------------------------------------------------------------

#[derive(Embed, Clone, Debug, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct KnowledgeDocument {
    pub id: String,
    #[embed]
    pub content: String,
    pub source: String,
    pub sensitivity: String,
}

impl SqliteVectorStoreTable for KnowledgeDocument {
    fn name() -> &'static str {
        "knowledge_docs"
    }
    fn schema() -> Vec<Column> {
        vec![
            Column::new("id", "TEXT PRIMARY KEY"),
            Column::new("content", "TEXT"),
            Column::new("source", "TEXT"),
            Column::new("sensitivity", "TEXT"),
        ]
    }
    fn id(&self) -> String {
        self.id.clone()
    }
    fn column_values(&self) -> Vec<(&'static str, Box<dyn ColumnValue>)> {
        vec![
            ("id", Box::new(self.id.clone())),
            ("content", Box::new(self.content.clone())),
            ("source", Box::new(self.source.clone())),
            ("sensitivity", Box::new(self.sensitivity.clone())),
        ]
    }
}

pub struct Tier3Search;

impl Tier3Search {
    pub fn search(
        &self,
        query_embedding: &[f32],
        db_path: &Path,
        top_k: usize,
    ) -> Result<Vec<SearchResult>> {
        if query_embedding.is_empty() {
            return Ok(Vec::new());
        }

        if !db_path.exists() {
            return Ok(Vec::new());
        }

        let conn = Connection::open(db_path)?;

        let top_k = top_k.max(1);
        let blob: Vec<u8> = query_embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        let rowids = {
            let mut stmt = conn
                .prepare("SELECT rowid FROM note_embeddings WHERE embedding MATCH ? AND k = ?")?;
            stmt.query_map(rusqlite::params![blob, top_k as u32], |row| {
                row.get::<_, u64>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };

        if rowids.is_empty() {
            return Ok(Vec::new());
        }

        let placeholders: Vec<String> = (1..=rowids.len()).map(|i| format!("?{i}")).collect();
        let query = format!(
            "SELECT nm.file_path, nf.content \
             FROM notes_meta nm \
             JOIN notes_fts nf ON nf.rowid = nm.rowid \
             WHERE nm.id IN ({}) \
             ORDER BY nm.rowid",
            placeholders.join(","),
        );

        let docs = {
            let mut stmt = conn.prepare(&query)?;
            stmt.query_map(
                rusqlite::params_from_iter(rowids.iter().map(|r| *r as i64)),
                |row| {
                    Ok(SearchResult {
                        file: std::path::PathBuf::from(row.get::<_, String>(0)?),
                        line: 0,
                        content: row.get::<_, String>(1)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };

        debug!(
            "Tier3Search: found {} results for {DIM}-dim query (top_k={top_k})",
            docs.len(),
        );

        Ok(docs)
    }

    pub fn insert_embedding(&self, db_path: &Path, note_id: &str, embedding: &[f32]) -> Result<()> {
        if embedding.is_empty() {
            anyhow::bail!("Cannot insert empty embedding for note {note_id}");
        }

        if !db_path.exists() {
            anyhow::bail!(
                "Vector database not found at {}. \
                 Initialize the database with init_vec_schema() first.",
                db_path.display()
            );
        }

        let conn = Connection::open(db_path)?;
        let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

        conn.execute(
            "INSERT OR REPLACE INTO note_embeddings (note_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![note_id, blob],
        )?;

        debug!(
            "Tier3Search: stored embedding for {note_id} ({}-dim)",
            embedding.len()
        );

        Ok(())
    }

    pub fn insert_entity_embedding(
        &self,
        db_path: &Path,
        entity_id: &str,
        embedding: &[f32],
    ) -> Result<()> {
        if embedding.is_empty() {
            anyhow::bail!("Cannot insert empty embedding for entity {entity_id}");
        }

        if !db_path.exists() {
            anyhow::bail!(
                "Vector database not found at {}. \
                 Initialize the database with init_vec_schema() first.",
                db_path.display()
            );
        }

        let conn = Connection::open(db_path)?;
        let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

        conn.execute(
            "INSERT OR REPLACE INTO entity_embeddings (entity_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![entity_id, blob],
        )?;

        debug!(
            "Tier3Search: stored entity embedding for {entity_id} ({}-dim)",
            embedding.len()
        );

        Ok(())
    }
}

impl std::fmt::Debug for Tier3Search {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tier3Search").finish()
    }
}

const DIM: usize = 384;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_tier3_search_empty_embedding_returns_empty() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("vec.db");
        let tier3 = Tier3Search;

        let results = tier3.search(&[], &db_path, 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_tier3_search_missing_db_returns_empty() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("nonexistent.db");
        let tier3 = Tier3Search;

        let results = tier3.search(&[1.0; DIM], &db_path, 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_tier3_insert_empty_embedding_fails() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("vec.db");
        let tier3 = Tier3Search;

        let result = tier3.insert_embedding(&db_path, "note-x", &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_tier3_insert_missing_db_fails_clearly() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("nonexistent.db");
        let tier3 = Tier3Search;

        let result = tier3.insert_embedding(&db_path, "note-x", &[1.0; DIM]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found") || err.contains("not available"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_knowledge_document_schema() {
        let doc = KnowledgeDocument {
            id: "test-1".to_string(),
            content: "Test content".to_string(),
            source: "test".to_string(),
            sensitivity: "private".to_string(),
        };

        assert_eq!(KnowledgeDocument::name(), "knowledge_docs");
        let schema = KnowledgeDocument::schema();
        assert_eq!(schema.len(), 4);
        assert_eq!(doc.id(), "test-1");
    }
}
