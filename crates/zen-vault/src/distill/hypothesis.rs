//! Jeff Dean Discovery Loop — hypothesis generation from gap records (FR-028, T029).
//!
//! Translates [`GapRecord`] detections into [`HypothesisSlug`] hypotheses,
//! persists them as markdown files with YAML frontmatter under
//! `wiki/wisdom/hypotheses/`, and provides refinement/re-verification
//! heuristics for the continuous knowledge loop.
//!
//! # Storage convention
//!
//! Each hypothesis is a `.md` file: YAML frontmatter (`---` delimited)
//! followed by a markdown body.  The filename is `{slug}.md`.
//! Idempotent merge semantics: re-saving the same slug appends new
//! evidence refs (deduplicated) and promotes status where appropriate.
//!
//! # Continuous refinement
//!
//! [`build_refinement_queue`] separates unresolved hypotheses into
//! external-fetch prompts and user-facing questions.  [`reverify`]
//! applies filesystem-based heuristics (evidence existence, entity
//! page presence) for the Compounding Synthesis stage; LLM re-verification
//! is deferred to a later integration task.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use tracing::debug;

use super::types::{GapKind, GapRecord, HypothesisSlug, HypothesisStatus};
use zen_memory::{extract_frontmatter, parse_field};

// ─── Gap taxonomy mapping (7 eligible kinds) ───────────────────────────

/// Map a [`GapKind`] to its short gap-type identifier for hypothesis slugs.
///
/// Returns `Some("…")` for the 7 gap kinds eligible for hypothesis generation
/// and `None` for the remaining 4 kinds that should be skipped with a debug log.
///
/// # Arguments
///
/// * `kind` — The gap taxonomy variant to classify.
///
/// # Returns
///
/// A static string identifier, or `None` if the kind is ineligible.
///
/// # Examples
///
/// ```
/// use crate::distill::hypothesis::gap_type;
/// use crate::distill::types::GapKind;
///
/// assert_eq!(gap_type(GapKind::WikiPageWithoutEntities), Some("missing_entity"));
/// assert_eq!(gap_type(GapKind::QuarantinedNote), None);
/// ```
pub fn gap_type(kind: GapKind) -> Option<&'static str> {
    match kind {
        GapKind::WikiPageWithoutEntities => Some("missing_entity"),
        GapKind::OrphanEntity => Some("orphan"),
        GapKind::UnresolvedRelationship => Some("missing_link"),
        GapKind::IngestNeverConsolidated => Some("stale_ingest"),
        GapKind::DuplicateEntityAlias => Some("alias_collision"),
        GapKind::DecisionBlocked => Some("decision_block"),
        GapKind::CommitmentOverdue => Some("commitment_overdue"),
        // Ineligible kinds — skipped during generation.
        GapKind::QuarantinedNote
        | GapKind::LlmFailure
        | GapKind::SelfCognitionBlocked
        | GapKind::AntiTalkSuspect => None,
    }
}

// ─── Confidence constants ──────────────────────────────────────────────

/// Return the base confidence score for a given gap kind.
///
/// Machine-verified structural graph gaps receive higher confidence because
/// their detection is deterministic (no LLM inference involved).  Decision
/// and commitment gaps carry moderate-to-high confidence based on rule
/// threshold violations.  Stale ingest is lower because the root cause may
/// be benign (user intentionally left the note untouched).
///
/// | Gap Kind                        | Confidence | Rationale                            |
/// |---------------------------------|------------|--------------------------------------|
/// | WikiPageWithoutEntities         | 0.70       | Structural: page exists, no entities |
/// | OrphanEntity                    | 0.70       | Structural: entity isolated in graph |
/// | UnresolvedRelationship          | 0.70       | Structural: dangling edge in graph   |
/// | DuplicateEntityAlias            | 0.60       | Deterministic alias collision        |
/// | DecisionBlocked                 | 0.65       | Rule-threshold violation             |
/// | CommitmentOverdue               | 0.60       | Deadline passed, rule-based          |
/// | IngestNeverConsolidated         | 0.50       | Possibly benign inaction             |
///
/// # Arguments
///
/// * `kind` — The gap taxonomy variant.
///
/// # Returns
///
/// Confidence score in `0.0..=1.0`.  The threshold for `Exploring` status is
/// `>= 0.6` (SC-012).
///
/// # Examples
///
/// ```
/// use crate::distill::hypothesis::confidence_for;
/// use crate::distill::types::GapKind;
///
/// assert!((confidence_for(GapKind::OrphanEntity) - 0.7).abs() < f64::EPSILON);
/// ```
pub fn confidence_for(kind: GapKind) -> f64 {
    match kind {
        // Structural graph gaps — deterministic detection → highest confidence.
        GapKind::WikiPageWithoutEntities
        | GapKind::OrphanEntity
        | GapKind::UnresolvedRelationship => 0.7,
        // Deterministic alias collision.
        GapKind::DuplicateEntityAlias => 0.6,
        // Rule-threshold violation — moderate-high.
        GapKind::DecisionBlocked => 0.65,
        // Deadline passed — rule-based.
        GapKind::CommitmentOverdue => 0.6,
        // Possibly benign — lowest confidence.
        GapKind::IngestNeverConsolidated => 0.5,
        // Ineligible kinds — return 0 (never used in generation).
        GapKind::QuarantinedNote
        | GapKind::LlmFailure
        | GapKind::SelfCognitionBlocked
        | GapKind::AntiTalkSuspect => 0.0,
    }
}

// ─── Slug generation ───────────────────────────────────────────────────

/// Normalize a raw string into a filesystem-safe kebab-case slug.
///
/// Converts to lowercase, replaces every non-alphanumeric character with `-`,
/// collapses runs of hyphens, and trims leading/trailing hyphens.  The
/// result is guaranteed to match the validation rules in
/// `crate::graph_router::validate_slug`.
fn to_kebab_slug(raw: &str) -> String {
    let mut slug: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    slug.trim_matches('-').to_string()
}

/// Generate a deterministic, stable slug for a hypothesis from a [`GapRecord`].
///
/// The slug format is `{gap_type}-{discriminant}` where the discriminant is
/// chosen in priority order:
///
/// 1. `subject_entity` (lowercased, kebab-cased)
/// 2. `subject_path` filename stem (lowercased, kebab-cased)
/// 3. First 8 hex characters of SHA-256(`gap.id`)
///
/// The resulting slug is truncated to 80 characters to stay within
/// common filesystem limits while remaining unique via the hash fallback.
///
/// # Arguments
///
/// * `gap` — The gap record to derive a slug from.
///
/// # Returns
///
/// A kebab-case string suitable for use as a filename (without `.md`).
///
/// # Examples
///
/// ```
/// use crate::distill::hypothesis::slug_for;
/// use crate::distill::types::{GapRecord, GapKind};
///
/// let gap = GapRecord::new(GapKind::OrphanEntity, "c1", "orphan: Foo")
///     .with_entity("Foo");
/// let s = slug_for(&gap);
/// assert!(s.starts_with("orphan-"));
/// ```
pub fn slug_for(gap: &GapRecord) -> String {
    let gt = gap_type(gap.kind).unwrap_or("unknown");

    let stem = gap
        .subject_entity
        .as_deref()
        .map(to_kebab_slug)
        .or_else(|| {
            gap.subject_path.as_deref().map(|p| {
                Path::new(p)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(to_kebab_slug)
                    .unwrap_or_else(|| id_hash8(&gap.id))
            })
        })
        .unwrap_or_else(|| id_hash8(&gap.id));

    let raw = format!("{gt}-{stem}");
    // Truncate to 80 chars; append hash suffix if truncated for uniqueness.
    if raw.len() > 80 {
        let hash_part = id_hash8(&gap.id);
        let max_stem = 80 - hash_part.len() - 1; // 1 for the separator
        format!("{}-{}", &raw[..max_stem.min(raw.len())], hash_part)
    } else {
        raw
    }
}

/// First 8 hex characters of SHA-256(`id`) — used as discriminant fallback.
fn id_hash8(id: &str) -> String {
    let hash = Sha256::digest(id.as_bytes());
    hash.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

// ─── Exploration prompts ───────────────────────────────────────────────

/// Build an LLM exploration prompt for a hypothesis.
///
/// The prompt is tailored to the gap kind and names the specific subject
/// entity or path so the LLM can formulate a precise investigation.
///
/// # Arguments
///
/// * `h` — The hypothesis to build a prompt for.
///
/// # Returns
///
/// A non-empty prompt string suitable for LLM consumption.
///
/// # Examples
///
/// ```
/// use crate::distill::hypothesis::build_exploration_prompt;
/// use crate::distill::types::{HypothesisSlug, GapKind, HypothesisStatus};
///
/// let h = HypothesisSlug {
///     slug: "orphan-foo".into(),
///     hypothesis: "Entity 'Foo' has no wiki page".into(),
///     gap_kind: GapKind::OrphanEntity,
///     confidence: 0.7,
///     status: HypothesisStatus::Exploring,
///     exploration_prompt: None,
///     evidence_refs: vec![],
///     created_from: "g1".into(),
/// };
/// let p = build_exploration_prompt(&h);
/// assert!(p.contains("Foo"));
/// ```
pub fn build_exploration_prompt(h: &HypothesisSlug) -> String {
    match h.gap_kind {
        GapKind::WikiPageWithoutEntities => format!(
            "The wiki page '{}' exists but has no linked database entities. \
             Investigate whether this page describes a distinct concept, tool, or topic \
             that warrants entity extraction. Identify the core concepts and suggest \
             entity names, types, and relationships.",
            h.slug
        ),
        GapKind::OrphanEntity => format!(
            "Entity '{}' is isolated in the knowledge graph with no wiki page \
             and no relationships. Research the entity's context: What domain does \
             it belong to? What related entities or wiki pages should it connect to? \
             Propose a wiki page structure and at least 2 relationships.",
            extract_subject_from_hypothesis(h)
        ),
        GapKind::UnresolvedRelationship => format!(
            "A relationship in the knowledge graph references a missing note or entity \
             related to '{}'. Trace what the relationship was intended to express and \
             identify the missing endpoint. Suggest whether to create the missing node \
             or remove the dangling relationship.",
            extract_subject_from_hypothesis(h)
        ),
        GapKind::IngestNeverConsolidated => format!(
            "A note at '{}' has been ingested but never consolidated into the knowledge \
             base across multiple processing cycles. Determine the note's content type \
             and propose a consolidation strategy: entity extraction, wiki page creation, \
             or archival if the content is not actionable.",
            extract_subject_from_hypothesis(h)
        ),
        GapKind::DuplicateEntityAlias => format!(
            "Multiple entities share a similar name or alias for '{}', causing a \
             collision in the knowledge graph. Investigate whether these are genuinely \
             distinct entities (e.g., 'Rust' the language vs 'rust' the corrosion process) \
             or duplicates that should be merged. Propose canonical naming.",
            extract_subject_from_hypothesis(h)
        ),
        GapKind::DecisionBlocked => format!(
            "A decision related to '{}' failed the quality gate (7-principles / \
             10-anti-patterns CRIT check). Analyze what caused the block: missing \
             evidence, logical gap, or anti-pattern detection. Suggest remediation \
             steps to unblock the decision.",
            extract_subject_from_hypothesis(h)
        ),
        GapKind::CommitmentOverdue => format!(
            "A commitment related to '{}' has passed its review deadline. Assess \
             whether the commitment is still relevant and achievable. If yes, propose \
             a revised timeline and the next concrete action. If not, recommend \
             formal abandonment or pivot.",
            extract_subject_from_hypothesis(h)
        ),
        // Ineligible kinds — should never be called but handle defensively.
        GapKind::QuarantinedNote
        | GapKind::LlmFailure
        | GapKind::SelfCognitionBlocked
        | GapKind::AntiTalkSuspect => format!(
            "Investigate the gap of type '{}' for hypothesis '{}'.",
            h.gap_kind.as_str(),
            h.slug
        ),
    }
}

/// Extract the human-readable subject from a hypothesis for prompt templates.
fn extract_subject_from_hypothesis(h: &HypothesisSlug) -> String {
    // The hypothesis text often contains the subject name — extract first meaningful chunk.
    // For orphan/missing_link types the slug itself encodes the subject.
    h.slug.split('-').collect::<Vec<_>>().join(" ")
}

// ─── Batch generation ──────────────────────────────────────────────────

/// Generate [`HypothesisSlug`]s from a batch of gap records.
///
/// Only eligible gap kinds (those with a [`gap_type`] mapping) are processed.
/// Each eligible gap produces one hypothesis with:
///
/// * **status**: `Exploring` if `confidence >= 0.6`, else `Hypothesis`
///   (SC-012 boundary).
/// * **exploration_prompt**: Built via [`build_exploration_prompt`].
/// * **evidence_refs**: `[subject_path]` if present, empty otherwise.
/// * **created_from**: The gap's `id` (uuid v7).
///
/// Deduplication: if multiple gaps produce the same slug, only the first is
/// kept.
///
/// # Arguments
///
/// * `gaps` — Slice of gap records from the current cycle's lint/verification stage.
/// * `now` — Current wall-clock time (injected for determinism in tests).
///
/// # Returns
///
/// A vec of hypotheses, one per unique eligible slug.
///
/// # Examples
///
/// ```
/// use chrono::Utc;
/// use crate::distill::hypothesis::generate_from_gaps;
/// use crate::distill::types::{GapRecord, GapKind};
///
/// let gaps = vec![
///     GapRecord::new(GapKind::OrphanEntity, "c1", "orphan: Foo").with_entity("Foo"),
/// ];
/// let hypotheses = generate_from_gaps(&gaps, Utc::now());
/// assert_eq!(hypotheses.len(), 1);
/// assert_eq!(hypotheses[0].status, crate::distill::types::HypothesisStatus::Exploring);
/// ```
pub fn generate_from_gaps(gaps: &[GapRecord], _now: DateTime<Utc>) -> Vec<HypothesisSlug> {
    let mut seen_slugs = HashSet::new();
    let mut out = Vec::new();

    for gap in gaps {
        let gt = match gap_type(gap.kind) {
            Some(gt) => gt,
            None => {
                debug!(
                    gap_kind = gap.kind.as_str(),
                    gap_id = %gap.id,
                    "skipping ineligible gap kind for hypothesis generation"
                );
                continue;
            }
        };

        let slug = slug_for(gap);
        if !seen_slugs.insert(slug.clone()) {
            debug!(
                slug = %slug,
                gap_id = %gap.id,
                "duplicate slug — skipping"
            );
            continue;
        }

        let confidence = confidence_for(gap.kind);
        let status = if confidence >= 0.6 {
            HypothesisStatus::Exploring
        } else {
            HypothesisStatus::Hypothesis
        };

        let hypothesis_text = format!("Gap '{}' detected: {}", gt, gap.detail);

        let mut h = HypothesisSlug {
            slug,
            hypothesis: hypothesis_text,
            gap_kind: gap.kind,
            confidence,
            status,
            exploration_prompt: None,
            evidence_refs: Vec::new(),
            created_from: gap.id.clone(),
        };

        // Set exploration prompt.
        h.exploration_prompt = Some(build_exploration_prompt(&h));

        // Set evidence refs from subject_path.
        if let Some(ref path) = gap.subject_path {
            h.evidence_refs.push(path.clone());
        }

        out.push(h);
    }

    out
}

// ─── Refinement queue ──────────────────────────────────────────────────

/// Build a refinement queue from existing hypotheses.
///
/// Separates unresolved hypotheses (status is neither `Validated` nor
/// `Rejected`) into two buckets:
///
/// 1. **External-fetch prompts** — hypotheses whose evidence_refs point to
///    files that should be fetched or re-read from disk.
/// 2. **User questions** — hypotheses requiring human judgment (e.g.,
///    ambiguous entities, conflicting evidence).
///
/// # Arguments
///
/// * `slugs` — All loaded hypotheses from [`load_all`].
///
/// # Returns
///
/// A tuple `(external_fetch_prompts, user_questions)`.
///
/// # Examples
///
/// ```
/// use crate::distill::hypothesis::build_refinement_queue;
/// use crate::distill::types::{HypothesisSlug, GapKind, HypothesisStatus};
///
/// let h = HypothesisSlug {
///     slug: "orphan-foo".into(),
///     hypothesis: "...".into(),
///     gap_kind: GapKind::OrphanEntity,
///     confidence: 0.7,
///     status: HypothesisStatus::Exploring,
///     exploration_prompt: Some("Check foo.md".into()),
///     evidence_refs: vec!["raw/foo.md".into()],
///     created_from: "g1".into(),
/// };
/// let (ext, user) = build_refinement_queue(&[h]);
/// assert_eq!(ext.len() + user.len(), 1);
/// ```
pub fn build_refinement_queue(slugs: &[HypothesisSlug]) -> (Vec<String>, Vec<String>) {
    let mut external_fetch = Vec::new();
    let mut user_questions = Vec::new();

    for h in slugs {
        // Only include unresolved hypotheses.
        match h.status {
            HypothesisStatus::Validated | HypothesisStatus::Rejected => continue,
            HypothesisStatus::Hypothesis | HypothesisStatus::Exploring => {}
        }

        // Build an external-fetch prompt if there are evidence refs.
        if !h.evidence_refs.is_empty() {
            let refs_str = h.evidence_refs.join(", ");
            external_fetch.push(format!(
                "Re-read evidence files [{refs_str}] and update hypothesis '{}': {}",
                h.slug,
                h.exploration_prompt.as_deref().unwrap_or("no prompt set"),
            ));
        }

        // Build a user question for unresolved hypotheses.
        user_questions.push(format!(
            "Is hypothesis '{}' still relevant? Current status: {:?}. Detail: {}",
            h.slug, h.status, h.hypothesis,
        ));
    }

    (external_fetch, user_questions)
}

// ─── File persistence ──────────────────────────────────────────────────

/// Serialize a hypothesis to markdown with YAML frontmatter.
fn to_markdown(h: &HypothesisSlug) -> String {
    let mut md = String::new();
    md.push_str("---\n");
    md.push_str(&format!("slug: {}\n", h.slug));
    md.push_str(&format!(
        "hypothesis: \"{}\"\n",
        h.hypothesis.replace('"', "\\\"")
    ));
    md.push_str(&format!("gap_kind: {}\n", h.gap_kind.as_str()));
    md.push_str(&format!("confidence: {:.4}\n", h.confidence));
    md.push_str(&format!("status: {}\n", status_to_str(h.status)));
    if let Some(ref prompt) = h.exploration_prompt {
        md.push_str(&format!(
            "exploration_prompt: \"{}\"\n",
            prompt.replace('"', "\\\"")
        ));
    }
    md.push_str("evidence_refs:\n");
    for ref_path in &h.evidence_refs {
        md.push_str(&format!("  - \"{}\"\n", ref_path.replace('"', "\\\"")));
    }
    md.push_str(&format!("created_from: {}\n", h.created_from));
    md.push_str("---\n\n");
    md.push_str(&format!("# Hypothesis: {}\n\n", h.hypothesis));
    if let Some(ref prompt) = h.exploration_prompt {
        md.push_str("## Exploration Prompt\n\n");
        md.push_str(prompt);
        md.push('\n');
    }
    md
}

fn status_to_str(s: HypothesisStatus) -> &'static str {
    match s {
        HypothesisStatus::Hypothesis => "hypothesis",
        HypothesisStatus::Exploring => "exploring",
        HypothesisStatus::Validated => "validated",
        HypothesisStatus::Rejected => "rejected",
    }
}

fn status_rank(s: HypothesisStatus) -> u8 {
    match s {
        HypothesisStatus::Hypothesis => 0,
        HypothesisStatus::Exploring => 1,
        HypothesisStatus::Rejected => 2,
        HypothesisStatus::Validated => 3,
    }
}

fn parse_status(s: &str) -> Option<HypothesisStatus> {
    match s {
        "hypothesis" => Some(HypothesisStatus::Hypothesis),
        "exploring" => Some(HypothesisStatus::Exploring),
        "validated" => Some(HypothesisStatus::Validated),
        "rejected" => Some(HypothesisStatus::Rejected),
        _ => None,
    }
}

fn parse_gap_kind(s: &str) -> Option<GapKind> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

/// Parse a hypothesis from a markdown file's content.
fn from_markdown(content: &str) -> Result<HypothesisSlug> {
    let fm = extract_frontmatter(content).ok_or_else(|| anyhow::anyhow!("missing frontmatter"))?;

    let slug = parse_field(&fm, "slug").ok_or_else(|| anyhow::anyhow!("missing slug field"))?;
    let hypothesis = parse_field(&fm, "hypothesis")
        .map(|s| s.trim_matches('"').to_string())
        .ok_or_else(|| anyhow::anyhow!("missing hypothesis field"))?;
    let gap_kind_str =
        parse_field(&fm, "gap_kind").ok_or_else(|| anyhow::anyhow!("missing gap_kind field"))?;
    let gap_kind = parse_gap_kind(&gap_kind_str)
        .ok_or_else(|| anyhow::anyhow!("invalid gap_kind: {}", gap_kind_str))?;
    let confidence: f64 = parse_field(&fm, "confidence")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.5);
    let status_str = parse_field(&fm, "status").unwrap_or_else(|| "hypothesis".to_string());
    let status = parse_status(&status_str).unwrap_or(HypothesisStatus::Hypothesis);
    let exploration_prompt =
        parse_field(&fm, "exploration_prompt").map(|s| s.trim_matches('"').to_string());
    let created_from = parse_field(&fm, "created_from").unwrap_or_default();

    // Parse evidence_refs from YAML array lines (`  - "path"`)
    let mut evidence_refs = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("- \"") && line.starts_with("  ") {
            let ref_path = trimmed
                .strip_prefix("- \"")
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(trimmed.strip_prefix("- ").unwrap_or(trimmed));
            evidence_refs.push(ref_path.to_string());
        }
    }

    Ok(HypothesisSlug {
        slug,
        hypothesis,
        gap_kind,
        confidence,
        status,
        exploration_prompt,
        evidence_refs,
        created_from,
    })
}

/// Save a hypothesis to `hypotheses_dir/{slug}.md`.
///
/// Idempotent merge: if a file already exists for this slug, the existing
/// hypothesis is loaded, new evidence_refs are appended (deduplicated), and
/// the higher status is kept (`Hypothesis` < `Exploring` < `Rejected` <
/// `Validated`).  `exploration_prompt` is filled if currently `None`.
///
/// # Arguments
///
/// * `h` — The hypothesis to persist.
/// * `hypotheses_dir` — Target directory (created if missing).
///
/// # Returns
///
/// The path to the written `.md` file.
///
/// # Errors
///
/// Returns an error if directory creation or file I/O fails.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use crate::distill::hypothesis::save;
/// use crate::distill::types::{HypothesisSlug, GapKind, HypothesisStatus};
///
/// let h = HypothesisSlug {
///     slug: "orphan-foo".into(),
///     hypothesis: "...".into(),
///     gap_kind: GapKind::OrphanEntity,
///     confidence: 0.7,
///     status: HypothesisStatus::Exploring,
///     exploration_prompt: None,
///     evidence_refs: vec![],
///     created_from: "g1".into(),
/// };
/// let path = save(&h, Path::new("/tmp/hypotheses")).unwrap();
/// assert!(path.ends_with("orphan-foo.md"));
/// ```
pub fn save(h: &HypothesisSlug, hypotheses_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(hypotheses_dir).with_context(|| {
        format!(
            "failed to create hypotheses dir: {}",
            hypotheses_dir.display()
        )
    })?;

    let path = hypotheses_dir.join(format!("{}.md", h.slug));

    // Idempotent merge: if file exists, merge evidence_refs and keep higher status.
    let mut merged = h.clone();
    if path.exists() {
        let existing_content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read existing hypothesis: {}", path.display()))?;
        if let Ok(existing) = from_markdown(&existing_content) {
            // Merge evidence refs (dedup).
            // Seed seen from new hypothesis (already in merged via clone).
            let mut seen: HashSet<&str> = h.evidence_refs.iter().map(|s| s.as_str()).collect();
            // Append existing refs not already present in the new set.
            for ref_path in &existing.evidence_refs {
                if seen.insert(ref_path) {
                    merged.evidence_refs.push(ref_path.clone());
                }
            }

            // Keep the higher status.
            if status_rank(existing.status) > status_rank(merged.status) {
                merged.status = existing.status;
            }

            // Fill exploration_prompt if None.
            if merged.exploration_prompt.is_none() {
                merged.exploration_prompt = existing.exploration_prompt;
            }
        }
    }

    let content = to_markdown(&merged);
    fs::write(&path, content)
        .with_context(|| format!("failed to write hypothesis: {}", path.display()))?;

    Ok(path)
}

/// Load all hypotheses from a directory of `.md` files.
///
/// Files that fail to parse are logged at `warn` level and skipped.
///
/// # Arguments
///
/// * `hypotheses_dir` — Directory containing `{slug}.md` files.
///
/// # Returns
///
/// A vec of successfully parsed hypotheses.
///
/// # Errors
///
/// Returns an error only if `read_dir` itself fails (e.g., permission denied).
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use crate::distill::hypothesis::load_all;
///
/// let hypotheses = load_all(Path::new("/tmp/hypotheses")).unwrap();
/// ```
pub fn load_all(hypotheses_dir: &Path) -> Result<Vec<HypothesisSlug>> {
    if !hypotheses_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut hypotheses = Vec::new();
    for entry in fs::read_dir(hypotheses_dir).with_context(|| {
        format!(
            "failed to read hypotheses dir: {}",
            hypotheses_dir.display()
        )
    })? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            match fs::read_to_string(&path) {
                Ok(content) => match from_markdown(&content) {
                    Ok(h) => hypotheses.push(h),
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "failed to parse hypothesis file, skipping"
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "failed to read hypothesis file, skipping"
                    );
                }
            }
        }
    }

    Ok(hypotheses)
}

// ─── Compounding Synthesis — re-verification heuristic ─────────────────

/// Re-verify old hypotheses using filesystem heuristics (Compounding Synthesis).
///
/// For each hypothesis with status `Hypothesis` or `Exploring` whose file
/// modification time is older than `now - older_than`:
///
/// * **All evidence_refs exist on disk** → transition to `Validated`
/// * **Subject entity page missing from wiki_dir** → transition to `Rejected`
/// * **Otherwise** → no change
///
/// LLM-based re-verification is a later integration task; this function
/// provides the deterministic filesystem baseline.
///
/// # Arguments
///
/// * `hypotheses_dir` — Directory containing hypothesis `.md` files.
/// * `wiki_dir` — Root of the wiki directory tree.
/// * `now` — Current wall-clock time (injected for determinism).
/// * `older_than` — Only re-verify hypotheses older than this duration.
///
/// # Returns
///
/// The count of hypotheses whose status was transitioned.
///
/// # Errors
///
/// Returns an error if directory I/O fails.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use chrono::Duration;
/// use crate::distill::hypothesis::reverify;
///
/// let count = reverify(
///     Path::new("/tmp/hypotheses"),
///     Path::new("/tmp/wiki"),
///     chrono::Utc::now(),
///     Duration::days(7),
/// ).unwrap();
/// ```
pub fn reverify(
    hypotheses_dir: &Path,
    wiki_dir: &Path,
    now: DateTime<Utc>,
    older_than: chrono::Duration,
) -> Result<usize> {
    let mut count = 0;
    let cutoff = now - older_than;

    if !hypotheses_dir.is_dir() {
        return Ok(0);
    }

    for entry in fs::read_dir(hypotheses_dir).with_context(|| {
        format!(
            "failed to read hypotheses dir: {}",
            hypotheses_dir.display()
        )
    })? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            // Use file mtime as the hypothesis creation/age proxy.
            let metadata = fs::metadata(&path)?;
            let mtime = metadata
                .modified()
                .map(DateTime::<Utc>::from)
                .unwrap_or(now);

            if mtime >= cutoff {
                continue;
            }

            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to read for reverify");
                    continue;
                }
            };

            let mut h = match from_markdown(&content) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to parse for reverify");
                    continue;
                }
            };

            // Only re-verify unresolved hypotheses.
            match h.status {
                HypothesisStatus::Validated | HypothesisStatus::Rejected => continue,
                HypothesisStatus::Hypothesis | HypothesisStatus::Exploring => {}
            }

            let mut transitioned = false;

            // Check 1: all evidence_refs exist on disk → Validated.
            if !h.evidence_refs.is_empty()
                && h.evidence_refs.iter().all(|ref_path| {
                    // Check both absolute and wiki_dir-relative.
                    Path::new(ref_path).exists() || wiki_dir.join(ref_path).exists()
                })
            {
                debug!(
                    slug = %h.slug,
                    old_status = ?h.status,
                    "evidence refs exist — transitioning to Validated"
                );
                h.status = HypothesisStatus::Validated;
                transitioned = true;
            }

            // Check 2: subject_entity page missing from wiki_dir → Rejected.
            if !transitioned && let Some(entity) = h.slug.split('-').collect::<Vec<_>>().last() {
                // Heuristic: look for any .md file in wiki_dir whose stem matches
                // the last segment of the slug (entity name).
                let entity_lower = entity.to_lowercase();
                let page_exists = walkdir::WalkDir::new(wiki_dir)
                    .max_depth(3)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .any(|e| {
                        e.path().extension().is_some_and(|ext| ext == "md")
                            && e.path()
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .map(|s| s.to_lowercase() == entity_lower)
                                .unwrap_or(false)
                    });

                if !page_exists {
                    debug!(
                        slug = %h.slug,
                        entity = %entity_lower,
                        "subject entity page missing — transitioning to Rejected"
                    );
                    h.status = HypothesisStatus::Rejected;
                    transitioned = true;
                }
            }

            if transitioned {
                // Persist the transition.
                save(&h, hypotheses_dir)?;
                count += 1;
            }
        }
    }

    Ok(count)
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distill::types::{GapKind, HypothesisStatus};
    use chrono::Duration;

    #[test]
    fn gap_type_mapping_completeness() {
        // 7 eligible kinds → Some
        assert_eq!(
            gap_type(GapKind::WikiPageWithoutEntities),
            Some("missing_entity")
        );
        assert_eq!(gap_type(GapKind::OrphanEntity), Some("orphan"));
        assert_eq!(
            gap_type(GapKind::UnresolvedRelationship),
            Some("missing_link")
        );
        assert_eq!(
            gap_type(GapKind::IngestNeverConsolidated),
            Some("stale_ingest")
        );
        assert_eq!(
            gap_type(GapKind::DuplicateEntityAlias),
            Some("alias_collision")
        );
        assert_eq!(gap_type(GapKind::DecisionBlocked), Some("decision_block"));
        assert_eq!(
            gap_type(GapKind::CommitmentOverdue),
            Some("commitment_overdue")
        );

        // 4 ineligible kinds → None
        assert_eq!(gap_type(GapKind::QuarantinedNote), None);
        assert_eq!(gap_type(GapKind::LlmFailure), None);
        assert_eq!(gap_type(GapKind::SelfCognitionBlocked), None);
        assert_eq!(gap_type(GapKind::AntiTalkSuspect), None);
    }

    #[test]
    fn confidence_boundary_0_6_exploring() {
        // 0.6 → Exploring (>= 0.6)
        let h_06 = HypothesisSlug {
            slug: "test".into(),
            hypothesis: "test".into(),
            gap_kind: GapKind::DuplicateEntityAlias, // confidence = 0.6
            confidence: confidence_for(GapKind::DuplicateEntityAlias),
            status: if confidence_for(GapKind::DuplicateEntityAlias) >= 0.6 {
                HypothesisStatus::Exploring
            } else {
                HypothesisStatus::Hypothesis
            },
            exploration_prompt: None,
            evidence_refs: vec![],
            created_from: "g1".into(),
        };
        assert_eq!(h_06.status, HypothesisStatus::Exploring);

        // 0.5 → Hypothesis (< 0.6)
        let h_05 = HypothesisSlug {
            slug: "test".into(),
            hypothesis: "test".into(),
            gap_kind: GapKind::IngestNeverConsolidated, // confidence = 0.5
            confidence: confidence_for(GapKind::IngestNeverConsolidated),
            status: if confidence_for(GapKind::IngestNeverConsolidated) >= 0.6 {
                HypothesisStatus::Exploring
            } else {
                HypothesisStatus::Hypothesis
            },
            exploration_prompt: None,
            evidence_refs: vec![],
            created_from: "g1".into(),
        };
        assert_eq!(h_05.status, HypothesisStatus::Hypothesis);
    }

    #[test]
    fn slug_stability() {
        let gap =
            GapRecord::new(GapKind::OrphanEntity, "cycle-1", "orphan: Foo").with_entity("Foo");
        let s1 = slug_for(&gap);
        let s2 = slug_for(&gap);
        assert_eq!(s1, s2);
        assert!(s1.starts_with("orphan-"));
        assert!(s1.contains("foo"));
    }

    #[test]
    fn slug_falls_back_to_id_hash_when_no_subject() {
        let gap = GapRecord::new(GapKind::WikiPageWithoutEntities, "c1", "no subject");
        let s = slug_for(&gap);
        assert!(s.starts_with("missing_entity-"));
        // Should contain 8-char hex hash
        let hash_part = s.strip_prefix("missing_entity-").unwrap();
        assert_eq!(hash_part.len(), 8);
        assert!(hash_part.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn slug_uses_path_stem_when_no_entity() {
        let gap = GapRecord::new(GapKind::IngestNeverConsolidated, "c1", "stale note")
            .with_path("raw/my-note-file.md");
        let s = slug_for(&gap);
        assert!(s.starts_with("stale_ingest-"));
        assert!(s.contains("my-note-file"));
    }

    #[test]
    fn generate_dedupe_same_entity() {
        let gap1 = GapRecord::new(GapKind::OrphanEntity, "c1", "orphan: Foo").with_entity("Foo");
        let gap2 =
            GapRecord::new(GapKind::OrphanEntity, "c2", "orphan: Foo again").with_entity("Foo");
        let now = Utc::now();
        let hypotheses = generate_from_gaps(&[gap1, gap2], now);
        // Same entity → same slug → deduped to 1
        assert_eq!(hypotheses.len(), 1);
    }

    #[test]
    fn generate_skips_ineligible_kinds() {
        let gaps = vec![
            GapRecord::new(GapKind::QuarantinedNote, "c1", "quarantined"),
            GapRecord::new(GapKind::LlmFailure, "c1", "llm failed"),
            GapRecord::new(GapKind::OrphanEntity, "c1", "orphan").with_entity("Bar"),
        ];
        let hypotheses = generate_from_gaps(&gaps, Utc::now());
        assert_eq!(hypotheses.len(), 1);
        assert_eq!(hypotheses[0].gap_kind, GapKind::OrphanEntity);
    }

    #[test]
    fn generate_sets_exploration_prompt_and_evidence() {
        let gap = GapRecord::new(GapKind::DecisionBlocked, "c1", "decision blocked")
            .with_path("decisions/q3.md")
            .with_entity("Q3 Plan");
        let expected_from = gap.id.clone();
        let hypotheses = generate_from_gaps(&[gap], Utc::now());
        assert_eq!(hypotheses.len(), 1);
        assert!(hypotheses[0].exploration_prompt.is_some());
        assert_eq!(hypotheses[0].evidence_refs, vec!["decisions/q3.md"]);
        assert_eq!(hypotheses[0].created_from, expected_from);
    }

    #[test]
    fn save_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let h = HypothesisSlug {
            slug: "test-roundtrip".into(),
            hypothesis: "Test roundtrip hypothesis".into(),
            gap_kind: GapKind::OrphanEntity,
            confidence: 0.7,
            status: HypothesisStatus::Exploring,
            exploration_prompt: Some("Investigate this entity.".into()),
            evidence_refs: vec!["raw/test.md".into()],
            created_from: "gap-123".into(),
        };

        let path = save(&h, tmp.path()).unwrap();
        assert!(path.exists());
        assert!(path.file_name().unwrap() == "test-roundtrip.md");

        let loaded = load_all(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        let lh = &loaded[0];
        assert_eq!(lh.slug, h.slug);
        assert_eq!(lh.hypothesis, h.hypothesis);
        assert_eq!(lh.gap_kind, h.gap_kind);
        assert!((lh.confidence - h.confidence).abs() < f64::EPSILON);
        assert_eq!(lh.status, h.status);
        assert_eq!(lh.exploration_prompt, h.exploration_prompt);
        assert_eq!(lh.evidence_refs, h.evidence_refs);
        assert_eq!(lh.created_from, h.created_from);
    }

    #[test]
    fn save_merge_keeps_higher_status() {
        let tmp = tempfile::tempdir().unwrap();
        let h1 = HypothesisSlug {
            slug: "merge-test".into(),
            hypothesis: "v1".into(),
            gap_kind: GapKind::OrphanEntity,
            confidence: 0.7,
            status: HypothesisStatus::Hypothesis,
            exploration_prompt: None,
            evidence_refs: vec!["raw/a.md".into()],
            created_from: "g1".into(),
        };
        save(&h1, tmp.path()).unwrap();

        // Re-save with higher status and new evidence.
        let h2 = HypothesisSlug {
            slug: "merge-test".into(),
            hypothesis: "v1".into(),
            gap_kind: GapKind::OrphanEntity,
            confidence: 0.7,
            status: HypothesisStatus::Exploring,
            exploration_prompt: Some("new prompt".into()),
            evidence_refs: vec!["raw/b.md".into()],
            created_from: "g2".into(),
        };
        save(&h2, tmp.path()).unwrap();

        let loaded = load_all(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        let lh = &loaded[0];
        // Kept the higher status (Exploring > Hypothesis).
        assert_eq!(lh.status, HypothesisStatus::Exploring);
        // Evidence refs merged and deduped.
        assert!(lh.evidence_refs.contains(&"raw/a.md".to_string()));
        assert!(lh.evidence_refs.contains(&"raw/b.md".to_string()));
        // Exploration prompt filled from new save (was None in old).
        assert_eq!(lh.exploration_prompt.as_deref(), Some("new prompt"));
    }

    #[test]
    fn save_merge_keeps_validated_over_exploring() {
        let tmp = tempfile::tempdir().unwrap();
        let h1 = HypothesisSlug {
            slug: "val-test".into(),
            hypothesis: "v1".into(),
            gap_kind: GapKind::OrphanEntity,
            confidence: 0.7,
            status: HypothesisStatus::Validated,
            exploration_prompt: None,
            evidence_refs: vec![],
            created_from: "g1".into(),
        };
        save(&h1, tmp.path()).unwrap();

        // Try to downgrade to Exploring.
        let h2 = HypothesisSlug {
            slug: "val-test".into(),
            hypothesis: "v2".into(),
            gap_kind: GapKind::OrphanEntity,
            confidence: 0.7,
            status: HypothesisStatus::Exploring,
            exploration_prompt: None,
            evidence_refs: vec![],
            created_from: "g2".into(),
        };
        save(&h2, tmp.path()).unwrap();

        let loaded = load_all(tmp.path()).unwrap();
        assert_eq!(loaded[0].status, HypothesisStatus::Validated);
    }

    #[test]
    fn load_all_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = load_all(tmp.path()).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn load_all_nonexistent_dir() {
        let loaded = load_all(Path::new("/nonexistent/path/xyz")).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn build_refinement_queue_filters_terminal() {
        let resolved = HypothesisSlug {
            slug: "resolved".into(),
            hypothesis: "done".into(),
            gap_kind: GapKind::OrphanEntity,
            confidence: 0.7,
            status: HypothesisStatus::Validated,
            exploration_prompt: None,
            evidence_refs: vec![],
            created_from: "g1".into(),
        };
        let unresolved = HypothesisSlug {
            slug: "open".into(),
            hypothesis: "still open".into(),
            gap_kind: GapKind::OrphanEntity,
            confidence: 0.7,
            status: HypothesisStatus::Exploring,
            exploration_prompt: Some("check again".into()),
            evidence_refs: vec!["raw/x.md".into()],
            created_from: "g2".into(),
        };
        let (ext, user) = build_refinement_queue(&[resolved, unresolved]);
        assert!(ext.iter().all(|p| p.contains("open")));
        assert!(user.iter().all(|q| q.contains("open")));
        assert!(!ext.iter().any(|p| p.contains("resolved")));
        assert!(!user.iter().any(|q| q.contains("resolved")));
    }

    #[test]
    fn reverify_transitions_all_evidence_present() {
        let tmp = tempfile::tempdir().unwrap();
        let hypo_dir = tmp.path().join("hypotheses");
        let wiki_dir = tmp.path().join("wiki");
        fs::create_dir_all(&hypo_dir).unwrap();
        fs::create_dir_all(&wiki_dir).unwrap();

        // Create evidence file.
        let evidence_path = tmp.path().join("evidence.md");
        fs::write(&evidence_path, "# evidence").unwrap();

        let h = HypothesisSlug {
            slug: "reverify-test".into(),
            hypothesis: "test".into(),
            gap_kind: GapKind::OrphanEntity,
            confidence: 0.7,
            status: HypothesisStatus::Exploring,
            exploration_prompt: None,
            evidence_refs: vec![evidence_path.to_string_lossy().to_string()],
            created_from: "g1".into(),
        };
        save(&h, &hypo_dir).unwrap();

        let hypo_file = hypo_dir.join("reverify-test.md");
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86400);
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&hypo_file)
            .unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(old_time))
            .unwrap();

        let count = reverify(&hypo_dir, &wiki_dir, Utc::now(), Duration::days(7)).unwrap();
        assert_eq!(count, 1);

        let loaded = load_all(&hypo_dir).unwrap();
        assert_eq!(loaded[0].status, HypothesisStatus::Validated);
    }

    #[test]
    fn reverify_no_transition_when_recent() {
        let tmp = tempfile::tempdir().unwrap();
        let hypo_dir = tmp.path().join("hypotheses");
        let wiki_dir = tmp.path().join("wiki");
        fs::create_dir_all(&hypo_dir).unwrap();
        fs::create_dir_all(&wiki_dir).unwrap();

        let h = HypothesisSlug {
            slug: "recent-test".into(),
            hypothesis: "test".into(),
            gap_kind: GapKind::OrphanEntity,
            confidence: 0.7,
            status: HypothesisStatus::Exploring,
            exploration_prompt: None,
            evidence_refs: vec![],
            created_from: "g1".into(),
        };
        save(&h, &hypo_dir).unwrap();

        // File is fresh — should not be reverified.
        let count = reverify(&hypo_dir, &wiki_dir, Utc::now(), Duration::days(7)).unwrap();
        assert_eq!(count, 0);

        let loaded = load_all(&hypo_dir).unwrap();
        assert_eq!(loaded[0].status, HypothesisStatus::Exploring);
    }
}
