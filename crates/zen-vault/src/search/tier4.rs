use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;
use tracing::debug;

use super::SearchResult;
use crate::tools::{
    SharedSqliteClient, ZenTool, ZenToolError, ZenToolResult, args_schema_entity,
    result_schema_array,
};
use zen_repo::{
    ComponentResult, GraphSearchResult, InsertRelationshipRequest, NotionsRepo, PageRankResult,
    ShortestPathResult, SqliteClient,
};

/// PageRank damping factor (standard value; 20 power iterations is far beyond
/// convergence for a personal knowledge graph's entity count).
const PPR_DAMPING: f64 = 0.85;
const PPR_ITERATIONS: usize = 20;

/// Upper bound on query terms tried as entity seeds, so a long query costs a
/// bounded number of lookups.
const MAX_SEEDS: usize = 6;

/// Query terms worth trying as entity seeds: alphanumeric runs of 3+ chars,
/// lowercased (matching the `COLLATE NOCASE` lookup), in first-seen order and
/// deduplicated so the seed set is deterministic.
fn query_seed_candidates(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in query.split(|c: char| !c.is_alphanumeric()) {
        if token.chars().count() < 3 {
            continue;
        }
        let lowered = token.to_lowercase();
        if !out.contains(&lowered) {
            out.push(lowered);
        }
    }
    out
}

/// A node in an extracted N-hop subgraph (FR-031 Sub-graph Synthesis RAG).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubgraphNode {
    /// Canonical entity name.
    pub name: String,
    /// BFS depth from the center entity (0 = center).
    pub depth: u32,
}

/// A directed edge between two discovered subgraph nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubgraphEdge {
    /// Source entity name.
    pub source: String,
    /// Target entity name.
    pub target: String,
    /// Relationship type as stored in the graph (e.g. `knows`, `RelatedTo`).
    pub relation: String,
}

/// N-hop subgraph centered on an entity, built for LLM prompt injection —
/// FR-031 Sub-graph Synthesis RAG: instead of Top-K vector retrieval, the
/// whole neighborhood is handed to the Agent for cross-document synthesis.
///
/// Tier-5/service wiring lands in a later wave; this type plus
/// [`Tier4Search::subgraph_synthesis`] and [`Tier4Search::synthesize_context`]
/// are the delivery surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubgraphContext {
    /// Center entity name the subgraph was extracted around.
    pub center: String,
    /// Discovered nodes (center included at depth 0 when found), sorted by
    /// `(depth, name)`.
    pub nodes: Vec<SubgraphNode>,
    /// Deduplicated exact edges whose both endpoints are in `nodes`, sorted
    /// by `(source, relation, target)`.
    pub edges: Vec<SubgraphEdge>,
}

impl SubgraphContext {
    /// Render a compact markdown-ish context block: a header line, nodes
    /// grouped by ascending BFS depth (name-sorted within a depth), then
    /// deterministically ordered edges. Suitable for direct injection into
    /// an LLM synthesis prompt.
    pub fn render(&self) -> String {
        let mut out = format!(
            "## Subgraph: {} ({} nodes, {} edges)\n",
            self.center,
            self.nodes.len(),
            self.edges.len()
        );

        let mut by_depth: BTreeMap<u32, Vec<&str>> = BTreeMap::new();
        for node in &self.nodes {
            by_depth
                .entry(node.depth)
                .or_default()
                .push(node.name.as_str());
        }
        for (depth, names) in by_depth {
            out.push_str(&format!("\n### Depth {depth}\n"));
            for name in names {
                out.push_str(&format!("- {name}\n"));
            }
        }

        out.push_str("\n### Edges\n");
        let mut lines: Vec<String> = self
            .edges
            .iter()
            .map(|e| format!("- {} -> {} ({})\n", e.source, e.target, e.relation))
            .collect();
        lines.sort();
        lines.dedup();
        for line in lines {
            out.push_str(&line);
        }
        out
    }
}

pub struct GraphResult {
    pub notion: String,
    pub depth: u32,
    pub relation: String,
    pub target: String,
    pub source_entity: String,
    pub direction: String,
}

impl From<GraphSearchResult> for GraphResult {
    fn from(r: GraphSearchResult) -> Self {
        GraphResult {
            notion: r.notion,
            depth: r.depth,
            relation: r.relation,
            target: r.target,
            source_entity: r.source_entity,
            direction: r.direction,
        }
    }
}

/// Tier 4 search: notion graph traversal with BFS.
#[derive(Debug)]
pub struct Tier4Search;

impl Tier4Search {
    pub async fn search(
        &self,
        client: &SqliteClient,
        notion_name: &str,
        max_depth: u32,
    ) -> Result<Vec<GraphResult>> {
        if notion_name.trim().is_empty() {
            return Ok(Vec::new());
        }

        let results = NotionsRepo::new(client)
            .bfs_search(notion_name, max_depth)
            .await?;

        let graph_results: Vec<GraphResult> = results.into_iter().map(GraphResult::from).collect();

        debug!(
            "Tier4Search: found {} notions for '{}' (depth={})",
            graph_results.len(),
            notion_name,
            max_depth
        );
        Ok(graph_results)
    }

    pub async fn insert_entity(
        &self,
        client: &SqliteClient,
        id: &str,
        name: &str,
        kind: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let repo = NotionsRepo::new(client);

        repo.insert_entity(id, name, kind, &now).await?;

        // Register normalized alias for notion deduplication.
        use unicode_normalization::UnicodeNormalization;
        let normalized: String = name.nfc().collect();
        let normalized = normalized.trim().to_lowercase();
        repo.insert_alias(&normalized, id).await?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_relationship(
        &self,
        client: &SqliteClient,
        id: &str,
        source_id: &str,
        target_id: &str,
        relation_type: &str,
        confidence: f64,
        source_note_ids: Option<&str>,
        created_at: &str,
    ) -> Result<()> {
        let req = InsertRelationshipRequest {
            id,
            source_id,
            target_id,
            rel_type: relation_type,
            confidence,
            source_note_ids,
            created_at,
            description: None,
            valid_from: None,
            valid_until: None,
            weight: None,
        };
        NotionsRepo::new(client).insert_relationship(&req).await?;
        Ok(())
    }

    pub async fn shortest_path(
        &self,
        client: &SqliteClient,
        src_name: &str,
        dst_name: &str,
        max_depth: u32,
    ) -> Result<Option<ShortestPathResult>> {
        NotionsRepo::new(client)
            .shortest_path(src_name, dst_name, max_depth)
            .await
            .map_err(Into::into)
    }

    pub async fn pagerank(
        &self,
        client: &SqliteClient,
        iterations: usize,
        damping: f64,
    ) -> Result<Vec<PageRankResult>> {
        NotionsRepo::new(client)
            .pagerank(iterations, damping)
            .await
            .map_err(Into::into)
    }

    pub async fn connected_components(
        &self,
        client: &SqliteClient,
    ) -> Result<Vec<ComponentResult>> {
        NotionsRepo::new(client)
            .connected_components()
            .await
            .map_err(Into::into)
    }

    /// Rank entities by query relevance with personalized PageRank seeded from
    /// the query's own entities (HippoRAG).
    ///
    /// [`Self::search`] needs an exact entity name as its seed, so a
    /// natural-language query reaches the graph only when it happens to name
    /// one. Here query terms are resolved through entity names and aliases
    /// first, and the graph is ranked *relative to those seeds* instead of
    /// globally. Seeds themselves are excluded from the output (they are
    /// already known to the caller); an empty seed set yields an empty result
    /// so the other retrieval tiers are unaffected.
    pub async fn seeded_ranking(
        &self,
        client: &SqliteClient,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let repo = NotionsRepo::new(client);
        let mut seeds: Vec<String> = Vec::new();
        for token in query_seed_candidates(query).into_iter().take(MAX_SEEDS) {
            let resolved = match repo.find_entity_by_name(&token).await {
                Ok(Some(row)) => Some(row.name),
                Ok(None) => match repo.resolve_alias(&token).await {
                    Ok(Some(id)) => repo.notion_name(&id).await.ok().flatten(),
                    _ => None,
                },
                Err(_) => None,
            };
            if let Some(name) = resolved
                && !seeds.contains(&name)
            {
                seeds.push(name);
            }
        }
        if seeds.is_empty() {
            return Ok(Vec::new());
        }

        let seeded_by = seeds.join(", ");
        let ranked = repo
            .personalized_pagerank(&seeds, PPR_ITERATIONS, PPR_DAMPING, 1.0 - PPR_DAMPING)
            .await?;
        Ok(ranked
            .into_iter()
            .filter(|row| !seeds.contains(&row.notion))
            .take(limit)
            .map(|row| SearchResult {
                file: PathBuf::from(format!("@{}", row.notion)),
                line: 0,
                content: format!("related to {seeded_by} (ppr {:.3})", row.score),
            })
            .collect())
    }

    /// Build the N-hop subgraph centered on `center` (FR-031 Sub-graph
    /// Synthesis RAG).
    ///
    /// Nodes come from [`NotionsRepo::bfs_search`] (deduplicated by name at
    /// minimum depth); exact edges come from
    /// [`NotionsRepo::load_relationships_all`] per discovered node, keeping
    /// only edges whose both endpoints are in the node set. Unknown or
    /// isolated centers yield an empty (but well-formed) context.
    ///
    /// Cost note: one entity-lookup plus one bidirectional edge query per
    /// discovered node — bounded by the subgraph size, which `hops` caps.
    pub async fn subgraph_synthesis(
        client: &SqliteClient,
        center: &str,
        hops: u32,
    ) -> Result<SubgraphContext> {
        if center.trim().is_empty() {
            return Ok(SubgraphContext {
                center: center.to_string(),
                nodes: Vec::new(),
                edges: Vec::new(),
            });
        }

        let repo = NotionsRepo::new(client);
        let bfs = repo.bfs_search(center, hops).await?;

        let mut nodes: Vec<SubgraphNode> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        if !bfs.is_empty() {
            nodes.push(SubgraphNode {
                name: center.to_string(),
                depth: 0,
            });
            seen.insert(center.to_string());
        }
        for row in &bfs {
            if seen.insert(row.notion.clone()) {
                nodes.push(SubgraphNode {
                    name: row.notion.clone(),
                    depth: row.depth,
                });
            }
        }

        let mut edges: Vec<SubgraphEdge> = Vec::new();
        let mut edge_keys: HashSet<(String, String, String)> = HashSet::new();
        for node in &nodes {
            let Some(entity) = repo.find_entity_by_name(&node.name).await? else {
                continue;
            };
            for rel in repo.load_relationships_all(&entity.id).await? {
                let (Some(source), Some(target)) = (
                    repo.notion_name(&rel.source_notion_id).await?,
                    repo.notion_name(&rel.target_notion_id).await?,
                ) else {
                    continue;
                };
                if !seen.contains(&source) || !seen.contains(&target) {
                    continue;
                }
                if edge_keys.insert((source.clone(), target.clone(), rel.relation_type.clone())) {
                    edges.push(SubgraphEdge {
                        source,
                        target,
                        relation: rel.relation_type,
                    });
                }
            }
        }

        nodes.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.name.cmp(&b.name)));
        edges.sort_by(|a, b| {
            a.source
                .cmp(&b.source)
                .then_with(|| a.relation.cmp(&b.relation))
                .then_with(|| a.target.cmp(&b.target))
        });

        debug!(
            "subgraph_synthesis: center `{}` hops={hops} → {} nodes / {} edges",
            center,
            nodes.len(),
            edges.len()
        );
        Ok(SubgraphContext {
            center: center.to_string(),
            nodes,
            edges,
        })
    }

    /// Thin wrapper over [`Self::subgraph_synthesis`]: build the N-hop
    /// subgraph and return [`SubgraphContext::render`] output ready for LLM
    /// prompt injection. Tier-5/service wiring lands in a later wave.
    pub async fn synthesize_context(
        client: &SqliteClient,
        center: &str,
        hops: u32,
    ) -> Result<String> {
        Ok(Self::subgraph_synthesis(client, center, hops)
            .await?
            .render())
    }
}

/// Agent-facing `tier4_search` tool bound to a workspace-resolved DB.
///
/// The DB path is injected at construction (via [`SharedSqliteClient`]);
/// invocations never open a client themselves — the pre-D7 impl opened
/// `./state.db` relative to the process CWD, which silently queried the
/// wrong (or a nonexistent) database.
pub struct Tier4SearchTool {
    db: SharedSqliteClient,
    inner: Tier4Search,
}

impl Tier4SearchTool {
    pub fn new(db: SharedSqliteClient) -> Self {
        Self {
            db,
            inner: Tier4Search,
        }
    }
}

impl ZenTool for Tier4SearchTool {
    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "tier4_search".to_string(),
            description: "Notion graph traversal using BFS from a starting notion.".to_string(),
            args_schema: args_schema_entity(),
            result_schema: result_schema_array(),
        }
    }

    async fn invoke(&self, args: Value) -> ZenToolResult {
        let notion_name = args
            .get("notion_name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ZenToolError::InvalidArgs("missing required field: notion_name".to_string())
            })?;
        let max_depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(3) as u32;

        let client = self.db.get().await.map_err(ZenToolError::ExecutionFailed)?;

        let results = self
            .inner
            .search(&client, notion_name, max_depth)
            .await
            .map_err(|e| ZenToolError::ExecutionFailed(e.to_string()))?;

        let formatted: Vec<Value> = results
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "notion": r.notion,
                    "depth": r.depth,
                    "relation": r.relation,
                    "target": r.target,
                    "source_entity": r.source_entity,
                    "direction": r.direction,
                })
            })
            .collect();

        Ok(serde_json::json!({ "notions": formatted }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn setup_test_db() -> (tempfile::TempDir, SqliteClient) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("state.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        (dir, client)
    }

    #[tokio::test]
    async fn test_tier4_empty_query_returns_empty() {
        let (_dir, client) = setup_test_db().await;
        let tier4 = Tier4Search;
        let results = tier4.search(&client, "test", 3).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn seeded_ranking_reaches_related_entities_from_a_natural_query() {
        let (_dir, client) = setup_test_db().await;
        let tier4 = Tier4Search;
        let now = chrono::Utc::now().to_rfc3339();
        for (id, name) in [("e1", "Alice"), ("e2", "Bob"), ("e3", "Charly")] {
            tier4
                .insert_entity(&client, id, name, "person")
                .await
                .unwrap();
        }
        tier4
            .insert_relationship(&client, "r1", "e1", "e2", "knows", 0.9, None, &now)
            .await
            .unwrap();
        tier4
            .insert_relationship(&client, "r2", "e2", "e3", "knows", 0.9, None, &now)
            .await
            .unwrap();

        // A query naming one entity reaches the entities around it, which the
        // exact-name BFS seed of `search` could not do for a bare phrase.
        let results = tier4
            .seeded_ranking(&client, "what do I know about Alice", 10)
            .await
            .unwrap();
        let notions: Vec<&str> = results.iter().filter_map(|r| r.file.to_str()).collect();
        assert!(
            notions.contains(&"@Bob"),
            "seed-relative ranking must surface the neighbour: {notions:?}"
        );
        assert!(
            !notions.contains(&"@Alice"),
            "the seed itself is excluded (already known): {notions:?}"
        );
        assert!(results[0].content.contains("related to Alice"));
    }

    #[tokio::test]
    async fn seeded_ranking_resolves_aliases() {
        let (_dir, client) = setup_test_db().await;
        let tier4 = Tier4Search;
        let now = chrono::Utc::now().to_rfc3339();
        tier4
            .insert_entity(&client, "e1", "Alice", "person")
            .await
            .unwrap();
        tier4
            .insert_entity(&client, "e2", "Bob", "person")
            .await
            .unwrap();
        tier4
            .insert_relationship(&client, "r1", "e1", "e2", "knows", 0.9, None, &now)
            .await
            .unwrap();
        NotionsRepo::new(&client)
            .insert_alias("ally", "e1")
            .await
            .unwrap();

        let results = tier4
            .seeded_ranking(&client, "ally update", 10)
            .await
            .unwrap();
        assert!(
            results.iter().any(|r| r.file.to_str() == Some("@Bob")),
            "alias-resolved seed must rank its neighbours"
        );
    }

    #[tokio::test]
    async fn seeded_ranking_is_empty_without_a_resolvable_seed() {
        let (_dir, client) = setup_test_db().await;
        let tier4 = Tier4Search;
        tier4
            .insert_entity(&client, "e1", "Alice", "person")
            .await
            .unwrap();

        // No term resolves ⇒ empty, so fusion simply omits the list.
        let results = tier4
            .seeded_ranking(&client, "zzz nothing here matches", 10)
            .await
            .unwrap();
        assert!(results.is_empty());

        // Terms shorter than the seed minimum are ignored entirely.
        let short = tier4.seeded_ranking(&client, "ab cd", 10).await.unwrap();
        assert!(short.is_empty());
    }

    #[tokio::test]
    async fn test_tier4_insert_and_search_graph() {
        let (_dir, client) = setup_test_db().await;
        let tier4 = Tier4Search;

        tier4
            .insert_entity(&client, "e1", "Alice", "person")
            .await
            .unwrap();
        tier4
            .insert_entity(&client, "e2", "Bob", "person")
            .await
            .unwrap();
        tier4
            .insert_entity(&client, "e3", "Charly", "person")
            .await
            .unwrap();

        let now = chrono::Utc::now().to_rfc3339();
        tier4
            .insert_relationship(&client, "r1", "e1", "e2", "knows", 0.9, None, &now)
            .await
            .unwrap();
        tier4
            .insert_relationship(&client, "r2", "e2", "e3", "knows", 0.8, None, &now)
            .await
            .unwrap();

        let results = tier4.search(&client, "Alice", 2).await.unwrap();
        assert!(results.len() >= 2);
    }

    #[tokio::test]
    async fn test_tier4_bfs_depth_limit() {
        let (_dir, client) = setup_test_db().await;
        let tier4 = Tier4Search;

        for i in 0..5 {
            tier4
                .insert_entity(&client, &format!("e{i}"), &format!("N{i}"), "node")
                .await
                .unwrap();
        }
        for i in 0..4 {
            let now = chrono::Utc::now().to_rfc3339();
            tier4
                .insert_relationship(
                    &client,
                    &format!("r{i}"),
                    &format!("e{i}"),
                    &format!("e{}", i + 1),
                    "next",
                    1.0,
                    None,
                    &now,
                )
                .await
                .unwrap();
        }

        let results_depth1 = tier4.search(&client, "N0", 1).await.unwrap();
        let results_depth3 = tier4.search(&client, "N0", 3).await.unwrap();
        assert!(results_depth3.len() >= results_depth1.len());
    }

    #[tokio::test]
    async fn test_tier4_tool_schema() {
        let tool = Tier4SearchTool::new(SharedSqliteClient::new(std::path::PathBuf::from(
            "unused.db",
        )));
        let schema = tool.schema();
        assert_eq!(schema.name, "tier4_search");
        assert!(schema.description.contains("BFS"));
        assert!(
            schema
                .args_schema
                .get("properties")
                .and_then(|p| p.get("db_path"))
                .is_none(),
            "db_path must not be an invocable arg — it was the CWD bug vector"
        );
    }

    #[tokio::test]
    async fn tier4_tool_uses_injected_db() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("state.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        let tier4 = Tier4Search;
        tier4
            .insert_entity(&client, "e1", "Alice", "person")
            .await
            .unwrap();
        tier4
            .insert_entity(&client, "e2", "Bob", "person")
            .await
            .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        tier4
            .insert_relationship(&client, "r1", "e1", "e2", "knows", 1.0, None, &now)
            .await
            .unwrap();

        let tool = Tier4SearchTool::new(SharedSqliteClient::new(db_path));
        let result = tool
            .invoke(serde_json::json!({ "notion_name": "Alice", "max_depth": 2 }))
            .await
            .unwrap();
        let notions = result["notions"].as_array().unwrap();
        assert!(!notions.is_empty(), "got: {notions:?}");
    }

    #[test]
    fn subgraph_render_groups_nodes_by_depth_and_sorts_edges() {
        let ctx = SubgraphContext {
            center: "Alice".to_string(),
            nodes: vec![
                SubgraphNode {
                    name: "Bob".to_string(),
                    depth: 1,
                },
                SubgraphNode {
                    name: "Alice".to_string(),
                    depth: 0,
                },
                SubgraphNode {
                    name: "Zed".to_string(),
                    depth: 2,
                },
                SubgraphNode {
                    name: "Carol".to_string(),
                    depth: 1,
                },
            ],
            edges: vec![
                SubgraphEdge {
                    source: "Bob".to_string(),
                    target: "Zed".to_string(),
                    relation: "knows".to_string(),
                },
                SubgraphEdge {
                    source: "Alice".to_string(),
                    target: "Bob".to_string(),
                    relation: "knows".to_string(),
                },
            ],
        };

        let rendered = ctx.render();
        assert!(rendered.starts_with("## Subgraph: Alice (4 nodes, 2 edges)\n"));
        // Depth sections in ascending order.
        let d0 = rendered.find("### Depth 0").unwrap();
        let d1 = rendered.find("### Depth 1").unwrap();
        let d2 = rendered.find("### Depth 2").unwrap();
        assert!(d0 < d1 && d1 < d2);
        // Within depth 1, names sorted: Bob before Carol.
        let bob = rendered.find("- Bob\n").unwrap();
        let carol = rendered.find("- Carol\n").unwrap();
        assert!(d1 < bob && bob < carol);
        // Edges present and sorted by source.
        let edge_a = rendered.find("- Alice -> Bob (knows)\n").unwrap();
        let edge_b = rendered.find("- Bob -> Zed (knows)\n").unwrap();
        let edges_header = rendered.find("### Edges\n").unwrap();
        assert!(edges_header < edge_a && edge_a < edge_b);
    }

    #[test]
    fn subgraph_render_empty_is_well_formed() {
        let ctx = SubgraphContext {
            center: "Nobody".to_string(),
            nodes: Vec::new(),
            edges: Vec::new(),
        };
        let rendered = ctx.render();
        assert!(rendered.starts_with("## Subgraph: Nobody (0 nodes, 0 edges)\n"));
        assert!(!rendered.contains("### Depth"));
        assert!(rendered.contains("### Edges\n"));
    }

    #[tokio::test]
    async fn subgraph_synthesis_builds_nodes_and_exact_edges() {
        let (_dir, client) = setup_test_db().await;
        let tier4 = Tier4Search;
        tier4
            .insert_entity(&client, "e1", "Alice", "person")
            .await
            .unwrap();
        tier4
            .insert_entity(&client, "e2", "Bob", "person")
            .await
            .unwrap();
        tier4
            .insert_entity(&client, "e3", "Charly", "person")
            .await
            .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        tier4
            .insert_relationship(&client, "r1", "e1", "e2", "knows", 1.0, None, &now)
            .await
            .unwrap();
        tier4
            .insert_relationship(&client, "r2", "e2", "e3", "knows", 1.0, None, &now)
            .await
            .unwrap();

        // 2-hop: all three nodes, both directed edges with exact endpoints.
        let ctx = Tier4Search::subgraph_synthesis(&client, "Alice", 2)
            .await
            .unwrap();
        assert_eq!(ctx.center, "Alice");
        let names: Vec<&str> = ctx.nodes.iter().map(|n| n.name.as_str()).collect();
        for expected in ["Alice", "Bob", "Charly"] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
        assert_eq!(ctx.edges.len(), 2);
        assert!(
            ctx.edges
                .iter()
                .any(|e| e.source == "Alice" && e.target == "Bob" && e.relation == "knows")
        );
        assert!(
            ctx.edges
                .iter()
                .any(|e| e.source == "Bob" && e.target == "Charly" && e.relation == "knows")
        );

        // 1-hop: depth-2 node and its edge are excluded.
        let ctx1 = Tier4Search::subgraph_synthesis(&client, "Alice", 1)
            .await
            .unwrap();
        assert!(!ctx1.nodes.iter().any(|n| n.name == "Charly"));
        assert!(ctx1.edges.iter().all(|e| e.target != "Charly"));

        // Unknown center → empty but well-formed context.
        let ctx0 = Tier4Search::subgraph_synthesis(&client, "Ghost", 2)
            .await
            .unwrap();
        assert!(ctx0.nodes.is_empty() && ctx0.edges.is_empty());
    }

    #[tokio::test]
    async fn synthesize_context_returns_rendered_block() {
        let (_dir, client) = setup_test_db().await;
        let tier4 = Tier4Search;
        tier4
            .insert_entity(&client, "e1", "Alice", "person")
            .await
            .unwrap();
        tier4
            .insert_entity(&client, "e2", "Bob", "person")
            .await
            .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        tier4
            .insert_relationship(&client, "r1", "e1", "e2", "knows", 1.0, None, &now)
            .await
            .unwrap();

        let text = Tier4Search::synthesize_context(&client, "Alice", 2)
            .await
            .unwrap();
        assert!(text.starts_with("## Subgraph: Alice"));
        assert!(text.contains("- Alice -> Bob (knows)"));
    }
}
