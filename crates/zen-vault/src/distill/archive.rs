//! MAP-elites-style multi-axis archive for hypotheses (AlphaEvolve pattern)
//! combined with Darwin-Gödel-Machine stepping stones: rejected hypotheses
//! are preserved and re-sampled as material for future recombinations.
//!
//! Classification is deterministic — axes derive from data already present
//! on the hypothesis record, so no LLM call is required to maintain the
//! archive.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::hypothesis::{gap_type, load_all};
use super::types::{GapKind, HypothesisSlug};
use zen_memory::{RejectedHypothesis, extract_frontmatter, parse_field};

/// Evidence-volume axis: how much supporting material the hypothesis carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceAxis {
    None,
    Thin,
    Solid,
}

impl EvidenceAxis {
    pub fn of(h: &HypothesisSlug) -> Self {
        match h.evidence_refs.len() {
            0 => Self::None,
            1..=2 => Self::Thin,
            _ => Self::Solid,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Thin => "thin",
            Self::Solid => "solid",
        }
    }
}

/// Gap-domain axis: which class of knowledge defect the hypothesis targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KindAxis {
    /// Graph/wiki structural integrity (missing pages, links, aliases).
    Structural,
    /// Judgment quality (decisions, self-model).
    Judgment,
    /// Process/flow hygiene (stale ingest, overdue commitments, talk ratio).
    Process,
}

impl KindAxis {
    pub fn of(kind: GapKind) -> Self {
        match kind {
            GapKind::WikiPageWithoutEntities
            | GapKind::OrphanEntity
            | GapKind::UnresolvedRelationship
            | GapKind::DuplicateEntityAlias => Self::Structural,
            GapKind::DecisionBlocked | GapKind::SelfCognitionBlocked => Self::Judgment,
            GapKind::IngestNeverConsolidated
            | GapKind::CommitmentOverdue
            | GapKind::AntiTalkSuspect
            | GapKind::QuarantinedNote
            | GapKind::LlmFailure => Self::Process,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Structural => "structural",
            Self::Judgment => "judgment",
            Self::Process => "process",
        }
    }
}

/// Novelty axis: whether a comparable claim was already falsified before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessAxis {
    Fresh,
    Revisited,
}

impl FreshnessAxis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Revisited => "revisited",
        }
    }
}

/// Behaviour descriptor (one MAP-elites cell) for a hypothesis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveCell {
    pub evidence: EvidenceAxis,
    pub kind: KindAxis,
    pub freshness: FreshnessAxis,
}

impl ArchiveCell {
    /// Classify a hypothesis into its deterministic cell. Single classifier
    /// shared by [`Archive::refresh`] and [`Archive::prioritize`] so candidate
    /// cells are directly comparable with archived ones.
    pub fn of(h: &HypothesisSlug, rejected: &[RejectedHypothesis]) -> Self {
        Self {
            evidence: EvidenceAxis::of(h),
            kind: KindAxis::of(h.gap_kind),
            freshness: if is_revisiting(h, rejected) {
                FreshnessAxis::Revisited
            } else {
                FreshnessAxis::Fresh
            },
        }
    }

    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}",
            self.evidence.as_str(),
            self.kind.as_str(),
            self.freshness.as_str()
        )
    }
}

/// One archived hypothesis with its cell and current score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveEntry {
    pub slug: String,
    pub cell: ArchiveCell,
    /// Selection score (hypothesis confidence today).
    pub score: f64,
    pub status: String,
}

/// The archive itself (file-backed JSON; corrupt input is quarantined aside
/// and degrades to empty so a bad write can never block a loop cycle).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Archive {
    pub entries: Vec<ArchiveEntry>,
}

impl Archive {
    /// Read the archive; a missing file yields an empty archive, and a
    /// corrupt/unparseable file is moved aside as
    /// `<name>.corrupt-<ts>` (preserving the bad bytes for diagnosis) before
    /// an empty archive is returned — a truncated write must never silently
    /// revert illumination ordering (the I10 closure) to input order.
    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str(&raw) {
            Ok(archive) => archive,
            Err(e) => {
                warn!(error = %e, path = %path.display(), "archive corrupt — quarantining and starting empty");
                quarantine_corrupt(path);
                Self::default()
            }
        }
    }

    /// Persist the archive atomically (tmp + fsync + rename, mirroring the
    /// reward sidecar discipline) so a reader never observes a half-written
    /// file and a crash cannot truncate the archive in place.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create archive dir {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self).context("serialize archive")?;
        let tmp = path.with_extension("json.tmp");
        let mut file =
            std::fs::File::create(&tmp).with_context(|| format!("create tmp {}", tmp.display()))?;
        file.write_all(json.as_bytes())
            .and_then(|_| file.sync_all())
            .with_context(|| format!("write+fsync tmp {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
        Ok(())
    }

    /// Rebuild the archive from the current hypothesis set, re-classifying
    /// every entry against the rejected-claim history. Idempotent.
    pub fn refresh(
        hypotheses_dir: &Path,
        rejected_dir: &Path,
        archive_path: &Path,
    ) -> Result<Self> {
        let hypotheses = load_all(hypotheses_dir)?;
        let rejected = load_rejected(rejected_dir);
        let entries = hypotheses
            .iter()
            .map(|h| ArchiveEntry {
                slug: h.slug.clone(),
                cell: ArchiveCell::of(h, &rejected),
                score: h.confidence,
                status: format!("{:?}", h.status).to_lowercase(),
            })
            .collect();
        let archive = Self { entries };
        archive.save(archive_path)?;
        Ok(archive)
    }

    /// Number of distinct occupied cells (MAP-elites coverage).
    pub fn cell_count(&self) -> usize {
        self.entries
            .iter()
            .map(|e| e.cell.key())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }

    /// Order `hypotheses` so under-illuminated regions of hypothesis space come
    /// first — the curriculum signal, choosing what to pursue from accumulated
    /// state instead of file order.
    ///
    /// Each hypothesis is classified into its deterministic cell via
    /// [`ArchiveCell::of`]; a cell already well represented in the archive, or
    /// already claimed by an earlier candidate in this same batch, ranks lower.
    /// Ties keep input order and an empty archive preserves input order
    /// exactly, so this reprioritises without changing which items are present.
    pub fn prioritize(
        &self,
        hypotheses: &[HypothesisSlug],
        rejected: &[RejectedHypothesis],
    ) -> Vec<HypothesisSlug> {
        let mut occupied: BTreeMap<String, usize> = BTreeMap::new();
        for entry in &self.entries {
            *occupied.entry(entry.cell.key()).or_default() += 1;
        }

        let mut claimed: BTreeMap<String, usize> = BTreeMap::new();
        let mut ranked: Vec<(usize, usize)> = Vec::with_capacity(hypotheses.len());
        for (index, hypothesis) in hypotheses.iter().enumerate() {
            let key = ArchiveCell::of(hypothesis, rejected).key();
            let crowdedness =
                occupied.get(&key).copied().unwrap_or(0) + claimed.get(&key).copied().unwrap_or(0);
            *claimed.entry(key).or_default() += 1;
            ranked.push((crowdedness, index));
        }
        ranked.sort_unstable();
        ranked
            .into_iter()
            .map(|(_, index)| hypotheses[index].clone())
            .collect()
    }
}

/// Parse rejected-hypothesis records from `wiki/wisdom/rejected/*.md`.
/// Unreadable records are skipped with a warning.
pub fn load_rejected(rejected_dir: &Path) -> Vec<RejectedHypothesis> {
    let Ok(entries) = std::fs::read_dir(rejected_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(fm) = extract_frontmatter(&content) else {
            continue;
        };
        let unquote = |v: String| v.trim().trim_matches('"').to_string();
        let (Some(claim), Some(falsifier)) = (
            parse_field(&fm, "claim").map(unquote),
            parse_field(&fm, "falsifier").map(unquote),
        ) else {
            warn!(path = %path.display(), "rejected record missing claim/falsifier — skipped");
            continue;
        };
        out.push(RejectedHypothesis {
            claim,
            falsifier,
            because: parse_field(&fm, "because").map(unquote).unwrap_or_default(),
            expiry: parse_field(&fm, "expiry").map(unquote).unwrap_or_default(),
        });
    }
    out.sort_by(|a, b| a.claim.cmp(&b.claim));
    out
}

/// Whether a hypothesis revisits ground already covered by a falsified claim
/// (same gap type and, when present, the same subject entity).
pub fn is_revisiting(h: &HypothesisSlug, rejected: &[RejectedHypothesis]) -> bool {
    let Some(gt) = gap_type(h.gap_kind) else {
        return false;
    };
    let entity = h.slug.split('-').next_back().unwrap_or_default();
    rejected.iter().any(|r| {
        r.claim.contains(gt) && (entity.is_empty() || r.claim.to_lowercase().contains(entity))
    })
}

/// Move a corrupt archive aside as `<name>.corrupt-<ts>` so the bad bytes are
/// preserved for diagnosis instead of being silently overwritten by the next
/// save. Best-effort: a failed quarantine is logged, never fatal.
fn quarantine_corrupt(path: &Path) {
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "archive.json".to_string());
    let dest = path.with_file_name(format!("{name}.corrupt-{ts}"));
    match std::fs::rename(path, &dest) {
        Ok(()) => warn!(path = %dest.display(), "corrupt archive quarantined"),
        Err(e) => warn!(
            error = %e,
            path = %path.display(),
            "failed to quarantine corrupt archive"
        ),
    }
}

/// Default archive location under the logs directory.
pub fn archive_path(logs_dir: &Path) -> PathBuf {
    logs_dir.join("hypothesis-archive.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distill::types::HypothesisStatus;

    fn hypothesis(slug: &str, kind: GapKind, confidence: f64, refs: usize) -> HypothesisSlug {
        HypothesisSlug {
            slug: slug.to_string(),
            hypothesis: format!("Gap '{}' detected: detail", gap_type(kind).unwrap_or("x")),
            gap_kind: kind,
            confidence,
            status: HypothesisStatus::Exploring,
            exploration_prompt: None,
            evidence_refs: (0..refs).map(|i| format!("ref-{i}.md")).collect(),
            created_from: "gap-1".to_string(),
        }
    }

    #[test]
    fn axes_bucket_deterministically() {
        assert_eq!(
            EvidenceAxis::of(&hypothesis("a", GapKind::OrphanEntity, 0.5, 0)),
            EvidenceAxis::None
        );
        assert_eq!(
            EvidenceAxis::of(&hypothesis("a", GapKind::OrphanEntity, 0.5, 2)),
            EvidenceAxis::Thin
        );
        assert_eq!(
            EvidenceAxis::of(&hypothesis("a", GapKind::OrphanEntity, 0.5, 3)),
            EvidenceAxis::Solid
        );
        assert_eq!(KindAxis::of(GapKind::OrphanEntity), KindAxis::Structural);
        assert_eq!(KindAxis::of(GapKind::DecisionBlocked), KindAxis::Judgment);
        assert_eq!(KindAxis::of(GapKind::CommitmentOverdue), KindAxis::Process);
    }

    #[test]
    fn prioritize_puts_under_illuminated_cells_first() {
        let crowded = hypothesis("crowded", GapKind::OrphanEntity, 0.7, 1);
        let archive = Archive {
            entries: vec![
                ArchiveEntry {
                    slug: "crowded".to_string(),
                    cell: ArchiveCell::of(&crowded, &[]),
                    score: 1.0,
                    status: "exploring".to_string(),
                },
                ArchiveEntry {
                    slug: "crowded-two".to_string(),
                    cell: ArchiveCell::of(&crowded, &[]),
                    score: 1.0,
                    status: "exploring".to_string(),
                },
            ],
        };
        let fresh = hypothesis("fresh", GapKind::DecisionBlocked, 0.7, 1);

        let ordered = archive.prioritize(&[crowded.clone(), fresh.clone()], &[]);
        assert_eq!(
            ordered[0].slug, "fresh",
            "empty cell outranks a crowded one"
        );
        assert_eq!(ordered[1].slug, "crowded");
    }

    #[test]
    fn prioritize_spreads_within_a_batch() {
        let same_cell = hypothesis("x1", GapKind::OrphanEntity, 0.7, 1);
        let archive = Archive {
            entries: vec![ArchiveEntry {
                slug: "x1".to_string(),
                cell: ArchiveCell::of(&same_cell, &[]),
                score: 1.0,
                status: "exploring".to_string(),
            }],
        };
        let repeat = hypothesis("x2", GapKind::OrphanEntity, 0.7, 1);
        let other_cell = hypothesis("y1", GapKind::DecisionBlocked, 0.7, 1);

        let ordered = archive.prioritize(
            &[same_cell.clone(), repeat.clone(), other_cell.clone()],
            &[],
        );
        assert_eq!(
            ordered.iter().map(|h| h.slug.as_str()).collect::<Vec<_>>(),
            vec!["y1", "x1", "x2"],
            "unoccupied cell first, then the already-claimed cell, then its repeat"
        );
    }

    #[test]
    fn prioritize_is_deterministic_and_preserves_input_when_empty() {
        let empty = Archive::default();
        let candidates = vec![
            hypothesis("a", GapKind::OrphanEntity, 0.7, 1),
            hypothesis("b", GapKind::DecisionBlocked, 0.7, 0),
            hypothesis("c", GapKind::OrphanEntity, 0.7, 3),
        ];
        let first = empty.prioritize(&candidates, &[]);
        let second = empty.prioritize(&candidates, &[]);
        assert_eq!(
            first.iter().map(|h| h.slug.clone()).collect::<Vec<_>>(),
            candidates
                .iter()
                .map(|h| h.slug.clone())
                .collect::<Vec<_>>(),
            "no archive knowledge ⇒ input order is preserved exactly"
        );
        assert_eq!(
            first.iter().map(|h| h.slug.clone()).collect::<Vec<_>>(),
            second.iter().map(|h| h.slug.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn revisiting_detected_from_rejected_history() {
        let rejected = vec![RejectedHypothesis {
            claim: "Gap 'orphan' detected: entity Foo unreachable".to_string(),
            falsifier: "missing wiki page for entity 'foo'".to_string(),
            because: "reverify".to_string(),
            expiry: "2026-01-01".to_string(),
        }];
        let revisited = hypothesis("orphan-foo", GapKind::OrphanEntity, 0.7, 1);
        let fresh = hypothesis("orphan-bar", GapKind::OrphanEntity, 0.7, 1);
        assert!(is_revisiting(&revisited, &rejected));
        assert!(!is_revisiting(&fresh, &rejected));
    }

    #[test]
    fn refresh_and_load_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let hypotheses_dir = dir.path().join("hypotheses");
        let rejected_dir = dir.path().join("rejected");
        std::fs::create_dir_all(&hypotheses_dir).unwrap();
        crate::distill::hypothesis::save(
            &hypothesis("orphan-foo", GapKind::OrphanEntity, 0.7, 1),
            &hypotheses_dir,
        )
        .unwrap();

        let path = archive_path(dir.path());
        let archive = Archive::refresh(&hypotheses_dir, &rejected_dir, &path).unwrap();
        assert_eq!(archive.entries.len(), 1);
        assert_eq!(archive.entries[0].cell.evidence, EvidenceAxis::Thin);
        assert_eq!(archive.entries[0].cell.freshness, FreshnessAxis::Fresh);

        let loaded = Archive::load(&path);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].slug, "orphan-foo");
    }

    #[test]
    fn corrupt_archive_is_quarantined_and_loads_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("archive.json");
        std::fs::write(&path, "{not json").unwrap();
        assert!(Archive::load(&path).entries.is_empty());
        // The corrupt bytes are moved aside, not left in place to be silently
        // overwritten by the next save.
        assert!(!path.exists(), "corrupt archive must be moved aside");
        let quarantined: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("archive.json.corrupt-"))
            .collect();
        assert_eq!(
            quarantined.len(),
            1,
            "exactly one quarantine file expected, got {quarantined:?}"
        );
    }

    #[test]
    fn save_is_atomic_and_leaves_no_tmp_residue() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("archive.json");
        let archive = Archive {
            entries: vec![ArchiveEntry {
                slug: "a".to_string(),
                cell: ArchiveCell {
                    evidence: EvidenceAxis::None,
                    kind: KindAxis::Structural,
                    freshness: FreshnessAxis::Fresh,
                },
                score: 0.5,
                status: "exploring".to_string(),
            }],
        };
        archive.save(&path).unwrap();
        assert!(path.exists());
        assert!(
            !dir.path().join("archive.json.tmp").exists(),
            "tmp file must be renamed away"
        );
        let loaded = Archive::load(&path);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].slug, "a");
    }
}
