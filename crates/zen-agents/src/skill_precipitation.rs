//! RSI skill precipitation (FR-037 Hybrid C, D02) — distills repeated
//! tool successes / repeated user corrections into file-based skills.
//!
//! Channel reuse (no new RPC): drafts are staged into the worker queue-file
//! channel (`logs/skill-confirmations.json`, same pattern as
//! `refinement-queue.json`) plus an audit event (`logs/audit.jsonl`); the
//! user confirms through the existing `zen skill` CLI surface. Only a
//! confirmed draft becomes `~/.zen/skills/<name>/SKILL.md`.
//!
//! Trigger seeding consumes `zen_memory::preference_triggers` directly —
//! zen-agents already depends on zen-memory, so the `&Preference` seam is
//! used as-is (no `&[String]` adapter needed).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use zen_memory::{Preference, preference_triggers};
use zen_vault::distill::trigram_jaccard;

use crate::skill_history::{SkillExecutionRecord, SkillHistory};

/// Minimum observations in one similarity cluster before a draft is staged
/// (Hybrid C: "≥2 similar tool successes or repeated user correction").
pub const MIN_OBSERVATIONS: usize = 2;

/// Cluster similarity for observation summaries (same scale as wiki merge).
pub const OBSERVATION_SIMILARITY: f64 = 0.72;

/// A record rates as a tool success at or above this quality (0-10 scale).
pub const SUCCESS_QUALITY_RATING: u8 = 7;

/// Marker inside a record's summaries signalling a user correction.
pub const CORRECTION_MARKER: &str = "corrected";

/// Markers that flag an observation as a recorded pitfall, so the rendered
/// SKILL.md can carry a Gotchas section (FR-040). Deliberately narrow: a false
/// positive files a clean success under Gotchas, which misleads the next run
/// more than an explicitly-empty section does.
const GOTCHA_MARKERS: &[&str] = &[
    CORRECTION_MARKER,
    "error",
    "failed",
    "retry",
    "avoid",
    "mistake",
];

/// Pending-confirm queue file in the logs dir (worker surface channel).
pub const DRAFT_QUEUE_FILE: &str = "skill-confirmations.json";

/// Upper bound on seeded preference triggers per draft.
const MAX_SEEDED_TRIGGERS: usize = 8;

/// A skill candidate distilled from execution history, awaiting confirmation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillDraft {
    pub name: String,
    pub description: String,
    pub triggers: Vec<String>,
    #[serde(default)]
    pub context_files: Vec<PathBuf>,
    #[serde(default)]
    pub prompt: String,
    /// Observation summaries the draft was distilled from (evidence trail).
    pub observations: Vec<String>,
}

/// Detects, stages and confirms skill drafts (Hybrid C lifecycle:
/// `draft → confirmed SKILL.md → hit enable`, data-model.md §Skill).
pub struct SkillPrecipitator {
    skills_dir: PathBuf,
    logs_dir: PathBuf,
}

impl SkillPrecipitator {
    pub fn new(skills_dir: PathBuf, logs_dir: PathBuf) -> Self {
        Self {
            skills_dir,
            logs_dir,
        }
    }

    /// Detect draft candidates from execution histories.
    ///
    /// Parameters:
    /// - `history`: execution-history reader rooted at the skills dir.
    /// - `preferences`: M4 preference triples; their
    ///   `preference_triggers` seed the draft's trigger list.
    /// - `names`: skill names to inspect (usually from `list_history_names`).
    ///
    /// Returns one draft per name meeting either signal — a cluster of ≥
    /// [`MIN_OBSERVATIONS`] similar successes, or ≥ [`MIN_OBSERVATIONS`]
    /// similar user corrections.
    pub fn detect(
        &self,
        history: &SkillHistory,
        names: &[String],
        preferences: &[Preference],
    ) -> Result<Vec<SkillDraft>> {
        let mut seeded: Vec<String> = Vec::new();
        for p in preferences {
            for t in preference_triggers(p) {
                if !seeded.contains(&t) {
                    seeded.push(t);
                }
            }
        }
        let seeded = &seeded[..seeded.len().min(MAX_SEEDED_TRIGGERS)];

        let mut drafts = Vec::new();
        for name in names {
            let records = history.get_history(name)?;
            let Some(cluster) = detect_cluster(&records) else {
                continue;
            };
            drafts.push(SkillDraft {
                name: name.clone(),
                description: build_description(&records, &cluster),
                triggers: seeded.to_vec(),
                context_files: Vec::new(),
                prompt: build_prompt(name, &cluster),
                observations: cluster,
            });
        }
        debug!(drafts = drafts.len(), "skill precipitation detection done");
        Ok(drafts)
    }

    /// Skill names that have an execution history in the skills dir
    /// (`<name>-history.jsonl` stems, `history` marker files excluded).
    pub fn list_history_names(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        if !self.skills_dir.is_dir() {
            return Ok(names);
        }
        for entry in fs::read_dir(&self.skills_dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|f| f.to_str()) else {
                continue;
            };
            if let Some(stem) = file_name.strip_suffix("-history.jsonl")
                && !stem.is_empty()
            {
                names.push(stem.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    /// Stage drafts for user confirmation (Hybrid C: first occurrence is
    /// never auto-promoted). Merges into the pending queue, skipping names
    /// already pending or already confirmed as SKILL.md. Returns the count
    /// of newly staged drafts.
    pub fn stage(&self, drafts: &[SkillDraft]) -> Result<usize> {
        let mut pending = self.pending_drafts()?;
        let mut staged = 0;
        for draft in drafts {
            if pending.iter().any(|p| p.name == draft.name)
                || self.confirmed_path(&draft.name).is_file()
            {
                continue;
            }
            pending.push(draft.clone());
            staged += 1;
            append_audit(
                &self.logs_dir,
                serde_json::json!({
                    "kind": "skill.draft.staged",
                    "skill": draft.name,
                    "observations": draft.observations.len(),
                }),
            )?;
            info!(skill = %draft.name, "skill draft staged, awaiting confirmation");
        }
        if staged > 0 {
            self.write_pending(&pending)?;
        }
        Ok(staged)
    }

    /// Pending (unconfirmed) drafts.
    pub fn pending_drafts(&self) -> Result<Vec<SkillDraft>> {
        let path = self.queue_path();
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read skill draft queue: {}", path.display()))?;
        Ok(serde_json::from_str(&content).unwrap_or_default())
    }

    /// Confirm a pending draft — Hybrid C gate passed; writes
    /// `~/.zen/skills/<name>/SKILL.md` (frontmatter
    /// `name/description/triggers/context_files`) and drops the pending
    /// entry. Errors when no draft with `name` is pending.
    pub fn confirm(&self, name: &str) -> Result<PathBuf> {
        let mut pending = self.pending_drafts()?;
        let pos = pending
            .iter()
            .position(|p| p.name == name)
            .ok_or_else(|| anyhow::anyhow!("no pending skill draft named '{name}'"))?;
        let draft = pending.remove(pos);
        self.write_pending(&pending)?;

        let path = self.confirmed_path(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create skill dir: {}", parent.display()))?;
        }
        fs::write(&path, render_skill_md(&draft))
            .with_context(|| format!("write SKILL.md: {}", path.display()))?;
        append_audit(
            &self.logs_dir,
            serde_json::json!({"kind": "skill.draft.confirmed", "skill": name}),
        )?;
        info!(skill = name, path = %path.display(), "skill confirmed");
        Ok(path)
    }

    /// Discard a pending draft without promoting it.
    pub fn reject(&self, name: &str) -> Result<bool> {
        let mut pending = self.pending_drafts()?;
        let before = pending.len();
        pending.retain(|p| p.name != name);
        let removed = pending.len() != before;
        if removed {
            self.write_pending(&pending)?;
            append_audit(
                &self.logs_dir,
                serde_json::json!({"kind": "skill.draft.rejected", "skill": name}),
            )?;
        }
        Ok(removed)
    }

    fn queue_path(&self) -> PathBuf {
        self.logs_dir.join(DRAFT_QUEUE_FILE)
    }

    fn confirmed_path(&self, name: &str) -> PathBuf {
        self.skills_dir.join(name).join("SKILL.md")
    }

    fn write_pending(&self, pending: &[SkillDraft]) -> Result<()> {
        fs::create_dir_all(&self.logs_dir)
            .with_context(|| format!("create logs dir: {}", self.logs_dir.display()))?;
        let json = serde_json::to_string_pretty(pending)?;
        fs::write(self.queue_path(), json)
            .with_context(|| format!("write skill draft queue: {}", self.queue_path().display()))
    }
}

/// Pick the ≥ [`MIN_OBSERVATIONS`] cluster of similar observations, trying
/// the success signal first, then the user-correction signal.
///
/// Clustering is greedy around the first qualifying record's summary —
/// deterministic for identical history files. The observation COUNT (not the
/// unique-text count) gates promotion: two byte-identical successes are two
/// independent observations, so dedup applies to the returned exemplar texts
/// only, never to the quorum check.
fn detect_cluster(records: &[SkillExecutionRecord]) -> Option<Vec<String>> {
    for is_success in [true, false] {
        let matching: Vec<&SkillExecutionRecord> = records
            .iter()
            .filter(|r| qualifies(r, is_success))
            .collect();
        let Some(seed) = matching.first() else {
            continue;
        };
        let seed_text = summary_of(seed);
        let mut cluster: Vec<String> = Vec::new();
        let mut observations = 0usize;
        for record in &matching {
            let text = summary_of(record);
            if trigram_jaccard(&seed_text, &text) >= OBSERVATION_SIMILARITY {
                observations += 1;
                if !cluster.contains(&text) {
                    cluster.push(text);
                }
            }
        }
        if observations >= MIN_OBSERVATIONS {
            return Some(cluster);
        }
    }
    None
}

fn qualifies(record: &SkillExecutionRecord, success: bool) -> bool {
    if success {
        record
            .quality_rating
            .is_some_and(|r| r >= SUCCESS_QUALITY_RATING)
    } else {
        let hay = summary_of(record).to_lowercase();
        hay.contains(CORRECTION_MARKER)
    }
}

fn summary_of(record: &SkillExecutionRecord) -> String {
    format!("{} {}", record.context_summary, record.result_summary)
        .trim()
        .to_string()
}

fn build_description(records: &[SkillExecutionRecord], cluster: &[String]) -> String {
    let corrections = records
        .iter()
        .filter(|r| summary_of(r).to_lowercase().contains(CORRECTION_MARKER))
        .count();
    if corrections >= MIN_OBSERVATIONS {
        format!("Distilled from {corrections} repeated user corrections")
    } else {
        format!(
            "Distilled from {} similar successful runs (quality ≥ {SUCCESS_QUALITY_RATING}/10)",
            cluster.len()
        )
    }
}

fn build_prompt(name: &str, cluster: &[String]) -> String {
    format!(
        "Apply the established `{name}` procedure. Proven approach from prior runs: {}",
        cluster.first().cloned().unwrap_or_default()
    )
}

/// Render the confirmed `SKILL.md` (D02 frontmatter contract:
/// `name/description/triggers/context_files`).
fn render_skill_md(draft: &SkillDraft) -> String {
    let mut md = String::from("---\n");
    md.push_str(&format!("name: {}\n", draft.name));
    md.push_str(&format!("description: {}\n", draft.description));
    if draft.triggers.is_empty() {
        md.push_str("triggers: []\n");
    } else {
        md.push_str(&format!("triggers: [{}]\n", draft.triggers.join(", ")));
    }
    if !draft.context_files.is_empty() {
        md.push_str("context_files:\n");
        for f in &draft.context_files {
            md.push_str(&format!("  - {}\n", f.display()));
        }
    }
    md.push_str("---\n\n");
    md.push_str(&format!("# {}\n\n", draft.name));
    if !draft.prompt.is_empty() {
        md.push_str(&draft.prompt);
        md.push_str("\n\n");
    }
    if !draft.observations.is_empty() {
        md.push_str("## Evidence\n\n");
        for obs in &draft.observations {
            md.push_str(&format!("- {obs}\n"));
        }
    }

    // FR-040: the auto-proposed SKILL.md carries a Gotchas section. It is
    // always present so the section is part of the skill contract; when the
    // evidence holds no correction or failure, that is stated rather than
    // left blank, so a reader can tell "none recorded" from "not rendered".
    md.push_str("## Gotchas\n\n");
    match gotchas_from(&draft.observations) {
        found if found.is_empty() => md.push_str(&format!(
            "None recorded — distilled from {} clean observation(s).\n",
            draft.observations.len()
        )),
        found => {
            for gotcha in found {
                md.push_str(&format!("- {gotcha}\n"));
            }
        }
    }
    md
}

/// Observations that record a pitfall rather than a clean success, matched
/// case-insensitively against [`GOTCHA_MARKERS`].
fn gotchas_from(observations: &[String]) -> Vec<&String> {
    observations
        .iter()
        .filter(|observation| {
            let lowered = observation.to_lowercase();
            GOTCHA_MARKERS.iter().any(|marker| lowered.contains(marker))
        })
        .collect()
}

pub(crate) fn append_audit(logs_dir: &Path, event: serde_json::Value) -> Result<()> {
    fs::create_dir_all(logs_dir)
        .with_context(|| format!("create logs dir: {}", logs_dir.display()))?;
    let path = logs_dir.join("audit.jsonl");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open audit log: {}", path.display()))?;
    let mut line = serde_json::to_string(&event)?;
    line.push('\n');
    file.write_all(line.as_bytes())
        .with_context(|| format!("append audit log: {}", path.display()))
}
