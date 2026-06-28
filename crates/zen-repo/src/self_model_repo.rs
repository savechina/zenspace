use sqlx::Row;

use crate::client::{Result, SqliteClient, SqliteError};
use crate::types::SelfNodeRow;

pub struct SelfModelRepo<'a> {
    client: &'a SqliteClient,
}

impl<'a> SelfModelRepo<'a> {
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
    }

    pub async fn upsert(&self, node: &SelfNodeRow) -> Result<()> {
        let node = node.clone();
        self.client
            .writer()
            .call(move |conn| {
                let is_explicit = node.is_explicit.map(|b| b as i32);
                let sufficient_for = serde_json::to_string(&node.sufficient_for).unwrap_or_default();
                let necessary_for = serde_json::to_string(&node.necessary_for).unwrap_or_default();
                let evidence_refs = serde_json::to_string(&node.evidence_refs).unwrap_or_default();

                conn.execute(
                    "INSERT OR REPLACE INTO self_nodes (
                        id, name, layer, description, domain,
                        is_explicit, sufficient_for, necessary_for, controllability,
                        humility_score, optionality_count, core_pursuit,
                        source, confidence, evidence_refs, created_at, updated_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                    rusqlite::params![
                        node.id, node.name, node.layer, node.description, node.domain,
                        is_explicit, sufficient_for, necessary_for, node.controllability,
                        node.humility_score, node.optionality_count, node.core_pursuit,
                        node.source, node.confidence, evidence_refs, node.created_at, node.updated_at
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn load_all(&self) -> Result<Vec<SelfNodeRow>> {
        let rows = sqlx::query(
            "SELECT id, name, layer, description, domain, \
                    is_explicit, sufficient_for, necessary_for, controllability, \
                    humility_score, optionality_count, core_pursuit, \
                    source, confidence, evidence_refs, created_at, updated_at \
             FROM self_nodes ORDER BY name",
        )
        .fetch_all(self.client.pool())
        .await?;

        rows.into_iter().map(|row| parse_self_node_row(&row)).collect()
    }

    pub async fn load_by_layer(&self, layer: &str) -> Result<Vec<SelfNodeRow>> {
        let rows = sqlx::query(
            "SELECT id, name, layer, description, domain, \
                    is_explicit, sufficient_for, necessary_for, controllability, \
                    humility_score, optionality_count, core_pursuit, \
                    source, confidence, evidence_refs, created_at, updated_at \
             FROM self_nodes WHERE layer = ?1 ORDER BY name",
        )
        .bind(layer)
        .fetch_all(self.client.pool())
        .await?;

        rows.into_iter().map(|row| parse_self_node_row(&row)).collect()
    }
}

fn parse_self_node_row(row: &sqlx::sqlite::SqliteRow) -> Result<SelfNodeRow> {
    let is_explicit_i32: Option<i32> = row.get("is_explicit");
    let sufficient_for_str: String = row.get("sufficient_for");
    let necessary_for_str: String = row.get("necessary_for");
    let evidence_refs_str: String = row.get("evidence_refs");

    Ok(SelfNodeRow {
        id: row.get("id"),
        name: row.get("name"),
        layer: row.get("layer"),
        description: row.get("description"),
        domain: row.get("domain"),
        is_explicit: is_explicit_i32.map(|b| b != 0),
        sufficient_for: serde_json::from_str(&sufficient_for_str).unwrap_or_default(),
        necessary_for: serde_json::from_str(&necessary_for_str).unwrap_or_default(),
        controllability: row.get("controllability"),
        humility_score: row.get("humility_score"),
        optionality_count: row.get("optionality_count"),
        core_pursuit: row.get("core_pursuit"),
        source: row.get("source"),
        confidence: row.get("confidence"),
        evidence_refs: serde_json::from_str(&evidence_refs_str).unwrap_or_default(),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}
