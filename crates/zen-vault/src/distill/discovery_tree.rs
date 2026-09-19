//! Discovery-tree logging (T140 prerequisite) — structured per-node,
//! per-attempt records so a future Dream-RSI replay scorer can reconstruct
//! which attempt descended from which, what policy chose it, and what the
//! outcome was.
//!
//! `logs/hypothesis-archive.json` and `loop-last-report.json` are
//! report-shaped, not tree-shaped: they cannot answer "which attempt spawned
//! this one, under which policy, with what outcome". This module is the tree
//! view over the same events — an append-only JSONL log at
//! `logs/discovery-tree.jsonl`, one record per attempt, following the same
//! append-only discipline as `logs/audit.jsonl` (via
//! [`zen_core::jsonl`]).
//!
//! # Policy mapping (the selector that produced each attempt)
//!
//! | node_kind    | policy              | producer |
//! |--------------|---------------------|----------|
//! | incubation   | `gap-driven`        | `hypothesis::generate_from_gaps_with_history` (zen-loop stage 5b) |
//! | incumbent    | `incumbent`         | per-cycle baseline candidate (zen-loop stage 5b) |
//! | refinement   | `illumination-ordered` | `build_refinement_queue_prioritized` (zen-loop stage 5c) |
//! | reverify     | `incumbent`         | `reverify_with_rejections` validated transitions (zen-loop stage 5c) |
//! | rejection    | `stepping-stone`    | `record_rejected_hypotheses` (zen-loop stage 5c) |
//! | arena-loss   | `arena-loss`        | `adversarial::run_arena` staged losses |
//!
//! The per-cycle **incumbent** node is recorded explicitly so a future replay
//! scorer can include the incumbent policy in its candidate set — that
//! inclusion is what yields the never-worse guarantee (a policy is only
//! adopted when its recorded tree beats the incumbent's baseline).
//!
//! # Parent links
//!
//! Incubation and incumbent nodes are roots (`parent_id: None`). Every later
//! attempt links to the most recent prior attempt for the same hypothesis
//! slug via [`DiscoveryTree::latest_for_slug`] — refinement → incubation,
//! reverify → refinement/incubation, rejection → reverify/refinement, and
//! arena-loss → the latest incumbent baseline node.
//!
//! # Fail-open discipline
//!
//! A write failure is logged by the caller and never fails a loop cycle; a
//! corrupt or schema-mismatched line is skipped with a `warn` by the reader.
//! Records are additive and self-describing (serde snake_case, `Option` for
//! absent fields) so a future scorer needs no schema change.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::warn;

/// Stable slug for the per-cycle incumbent baseline node. A replay scorer
/// looks this up to include the incumbent policy in its candidate set.
pub const INCUMBENT_SLUG: &str = "incumbent";

/// The selector that produced an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// Gap-driven incubation (`generate_from_gaps_with_history`).
    GapDriven,
    /// `Archive::prioritize` curriculum ordering
    /// (`build_refinement_queue_prioritized`).
    IlluminationOrdered,
    /// A falsified attempt re-surfaced as recombination material for future
    /// generations (the rejection record itself).
    SteppingStone,
    /// Arena regression loss staged as an improvement hypothesis.
    ArenaLoss,
    /// The incumbent pipeline (current distill behavior) — the baseline
    /// candidate a replay scorer must include for the never-worse guarantee.
    Incumbent,
}

/// The kind of node in the discovery tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A hypothesis incubated from gaps (or the per-cycle incumbent baseline).
    Incubation,
    /// An unresolved hypothesis re-queued by the refinement curriculum.
    Refinement,
    /// A hypothesis transitioned by the reverify pass.
    Reverify,
    /// A challenger hypothesis staged from an arena loss.
    ArenaLoss,
    /// A falsified attempt recorded as negative space.
    Rejection,
}

/// The recorded outcome of an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The attempt's evidence was satisfied (reverify → Validated).
    Validated,
    /// The attempt was falsified (reverify → Rejected).
    Rejected,
    /// The attempt is still open (incubation, refinement, arena-loss,
    /// incumbent baseline).
    Pending,
    /// The attempt was superseded by a later one.
    Superseded,
}

/// One attempt in the discovery tree — one JSONL line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryNode {
    /// Unique attempt id (uuid v7).
    pub id: String,
    /// RFC3339 wall-clock timestamp of the attempt.
    pub ts: String,
    /// The attempt/node that spawned this one; `None` for roots.
    pub parent_id: Option<String>,
    /// Which kind of decision point produced the attempt.
    pub node_kind: NodeKind,
    /// The hypothesis this attempt concerns (`INCUMBENT_SLUG` for the
    /// per-cycle incumbent baseline).
    pub hypothesis_slug: String,
    /// The selector that produced the attempt.
    pub policy: Policy,
    /// The recorded outcome of the attempt.
    pub outcome: Outcome,
    /// Why the attempt failed, when rejected.
    pub falsifier: Option<String>,
    /// Vault-relative evidence file paths backing the attempt.
    pub evidence_refs: Vec<String>,
}

impl DiscoveryNode {
    /// Root incubation node: a hypothesis generated from gaps this cycle
    /// (policy `gap-driven`, outcome `pending`).
    pub fn incubation(slug: &str, evidence_refs: Vec<String>) -> Self {
        Self::new(
            NodeKind::Incubation,
            Policy::GapDriven,
            slug,
            None,
            Outcome::Pending,
            None,
            evidence_refs,
        )
    }

    /// Per-cycle incumbent baseline candidate (policy `incumbent`, slug
    /// [`INCUMBENT_SLUG`], outcome `pending`). Recorded at incubation so a
    /// replay scorer can include the incumbent in its candidate set.
    pub fn incumbent() -> Self {
        Self::new(
            NodeKind::Incubation,
            Policy::Incumbent,
            INCUMBENT_SLUG,
            None,
            Outcome::Pending,
            None,
            Vec::new(),
        )
    }

    /// Refinement-queue node: an unresolved hypothesis re-queued by the
    /// illumination-ordered curriculum (policy `illumination-ordered`,
    /// outcome `pending`).
    pub fn refinement(slug: &str, parent_id: Option<String>, evidence_refs: Vec<String>) -> Self {
        Self::new(
            NodeKind::Refinement,
            Policy::IlluminationOrdered,
            slug,
            parent_id,
            Outcome::Pending,
            None,
            evidence_refs,
        )
    }

    /// Reverify transition node — the incumbent pipeline acting (policy
    /// `incumbent`). Outcome is `validated` or `rejected` per the transition.
    pub fn reverify(
        slug: &str,
        parent_id: Option<String>,
        outcome: Outcome,
        falsifier: Option<String>,
        evidence_refs: Vec<String>,
    ) -> Self {
        Self::new(
            NodeKind::Reverify,
            Policy::Incumbent,
            slug,
            parent_id,
            outcome,
            falsifier,
            evidence_refs,
        )
    }

    /// Arena-loss node: a challenger hypothesis staged from a lost case
    /// (policy `arena-loss`, outcome `pending`), parented to the latest
    /// incumbent baseline node.
    pub fn arena_loss(slug: &str, parent_id: Option<String>, evidence_refs: Vec<String>) -> Self {
        Self::new(
            NodeKind::ArenaLoss,
            Policy::ArenaLoss,
            slug,
            parent_id,
            Outcome::Pending,
            None,
            evidence_refs,
        )
    }

    /// Rejection node: a falsified attempt recorded as negative space
    /// (policy `stepping-stone`, outcome `rejected`).
    pub fn rejection(
        slug: &str,
        parent_id: Option<String>,
        falsifier: String,
        evidence_refs: Vec<String>,
    ) -> Self {
        Self::new(
            NodeKind::Rejection,
            Policy::SteppingStone,
            slug,
            parent_id,
            Outcome::Rejected,
            Some(falsifier),
            evidence_refs,
        )
    }

    fn new(
        node_kind: NodeKind,
        policy: Policy,
        slug: &str,
        parent_id: Option<String>,
        outcome: Outcome,
        falsifier: Option<String>,
        evidence_refs: Vec<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            ts: chrono::Utc::now().to_rfc3339(),
            parent_id,
            node_kind,
            hypothesis_slug: slug.to_string(),
            policy,
            outcome,
            falsifier,
            evidence_refs,
        }
    }
}

/// Append-only discovery-tree store. Stateless: every method takes the log
/// path explicitly so call sites own their fail-open handling.
pub struct DiscoveryTree;

impl DiscoveryTree {
    /// Append one node as a JSONL line (creates the file and parent dirs).
    /// Fail-open is the caller's responsibility: a write failure is logged
    /// and never fails a loop cycle.
    pub fn append(path: &Path, node: &DiscoveryNode) -> Result<()> {
        zen_core::jsonl::append_jsonl_line(path, node)
    }

    /// Read every node in file order. A missing file yields an empty tree; a
    /// corrupt line is skipped with a `warn` (via [`zen_core::jsonl`]); a
    /// valid-JSON-but-schema-mismatched line is skipped with a `warn` here.
    /// Never fails — the tree is advisory, not load-bearing.
    pub fn load(path: &Path) -> Vec<DiscoveryNode> {
        let values = match zen_core::jsonl::read_jsonl_lines(path) {
            Ok(values) => values,
            Err(e) => {
                warn!(
                    error = %e,
                    path = %path.display(),
                    "discovery tree unreadable — starting empty"
                );
                return Vec::new();
            }
        };
        let mut nodes = Vec::with_capacity(values.len());
        for value in values {
            match serde_json::from_value::<DiscoveryNode>(value) {
                Ok(node) => nodes.push(node),
                Err(e) => warn!(
                    error = %e,
                    path = %path.display(),
                    "discovery tree line skipped (schema mismatch)"
                ),
            }
        }
        nodes
    }

    /// The most recent node for a hypothesis slug (any kind). The log is
    /// append-ordered, so reverse scan finds the latest attempt — the
    /// genuine parent for a descendant node.
    pub fn latest_for_slug<'a>(
        nodes: &'a [DiscoveryNode],
        slug: &str,
    ) -> Option<&'a DiscoveryNode> {
        nodes.iter().rev().find(|n| n.hypothesis_slug == slug)
    }
}

/// Default discovery-tree location under the logs directory.
pub fn discovery_tree_path(logs_dir: &Path) -> PathBuf {
    logs_dir.join("discovery-tree.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn append_records_well_formed_node_with_parent_and_outcome() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = discovery_tree_path(dir.path());

        let parent = DiscoveryNode::incubation("orphan-foo", vec!["raw/foo.md".to_string()]);
        DiscoveryTree::append(&path, &parent).unwrap();
        let child = DiscoveryNode::refinement("orphan-foo", Some(parent.id.clone()), Vec::new());
        DiscoveryTree::append(&path, &child).unwrap();

        let nodes = DiscoveryTree::load(&path);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].node_kind, NodeKind::Incubation);
        assert_eq!(nodes[0].policy, Policy::GapDriven);
        assert_eq!(nodes[0].outcome, Outcome::Pending);
        assert_eq!(nodes[0].parent_id, None);
        assert_eq!(nodes[0].evidence_refs, vec!["raw/foo.md".to_string()]);
        // RFC3339 timestamp (chrono emits `+00:00` for UTC, not `Z`).
        assert!(
            chrono::DateTime::parse_from_rfc3339(&nodes[0].ts).is_ok(),
            "ts must be RFC3339, got {:?}",
            nodes[0].ts
        );
        assert_eq!(nodes[1].node_kind, NodeKind::Refinement);
        assert_eq!(nodes[1].policy, Policy::IlluminationOrdered);
        assert_eq!(nodes[1].outcome, Outcome::Pending);
        assert_eq!(nodes[1].parent_id.as_deref(), Some(parent.id.as_str()));
    }

    #[test]
    fn corrupt_line_is_skipped_with_warning() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = discovery_tree_path(dir.path());
        DiscoveryTree::append(&path, &DiscoveryNode::incubation("a", Vec::new())).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{not json\n")
            .unwrap();
        DiscoveryTree::append(&path, &DiscoveryNode::incubation("b", Vec::new())).unwrap();

        let nodes = DiscoveryTree::load(&path);
        assert_eq!(nodes.len(), 2, "corrupt line must be skipped, not fatal");
        assert_eq!(nodes[0].hypothesis_slug, "a");
        assert_eq!(nodes[1].hypothesis_slug, "b");
    }

    #[test]
    fn schema_mismatched_line_is_skipped() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = discovery_tree_path(dir.path());
        DiscoveryTree::append(&path, &DiscoveryNode::incubation("a", Vec::new())).unwrap();
        // Valid JSON, wrong shape (a bare string) — must be skipped with warn.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\"not-a-node\"\n")
            .unwrap();

        let nodes = DiscoveryTree::load(&path);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].hypothesis_slug, "a");
    }

    #[test]
    fn incumbent_policy_recorded_at_incubation() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = discovery_tree_path(dir.path());
        DiscoveryTree::append(&path, &DiscoveryNode::incumbent()).unwrap();

        let nodes = DiscoveryTree::load(&path);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].policy, Policy::Incumbent);
        assert_eq!(nodes[0].hypothesis_slug, INCUMBENT_SLUG);
        assert_eq!(nodes[0].node_kind, NodeKind::Incubation);
        assert_eq!(nodes[0].outcome, Outcome::Pending);
        assert_eq!(nodes[0].parent_id, None);
    }

    #[test]
    fn refinement_parent_resolves_to_parent_attempt() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = discovery_tree_path(dir.path());
        let parent = DiscoveryNode::incubation("orphan-foo", Vec::new());
        DiscoveryTree::append(&path, &parent).unwrap();

        let nodes = DiscoveryTree::load(&path);
        let parent_id = DiscoveryTree::latest_for_slug(&nodes, "orphan-foo")
            .unwrap()
            .id
            .clone();
        let child = DiscoveryNode::refinement("orphan-foo", Some(parent_id), Vec::new());
        DiscoveryTree::append(&path, &child).unwrap();

        let nodes = DiscoveryTree::load(&path);
        let child = DiscoveryTree::latest_for_slug(&nodes, "orphan-foo").unwrap();
        assert_eq!(child.node_kind, NodeKind::Refinement);
        let parent = nodes
            .iter()
            .find(|n| n.id == child.parent_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(parent.node_kind, NodeKind::Incubation);
        assert_eq!(parent.hypothesis_slug, "orphan-foo");
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = discovery_tree_path(dir.path());
        assert!(DiscoveryTree::load(&path).is_empty());
    }

    #[test]
    fn latest_for_slug_prefers_most_recent_attempt() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = discovery_tree_path(dir.path());
        DiscoveryTree::append(&path, &DiscoveryNode::incubation("x", Vec::new())).unwrap();
        DiscoveryTree::append(&path, &DiscoveryNode::refinement("x", None, Vec::new())).unwrap();

        let nodes = DiscoveryTree::load(&path);
        let latest = DiscoveryTree::latest_for_slug(&nodes, "x").unwrap();
        assert_eq!(latest.node_kind, NodeKind::Refinement);
        assert!(DiscoveryTree::latest_for_slug(&nodes, "absent").is_none());
    }
}
